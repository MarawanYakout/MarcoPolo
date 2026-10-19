//! Concurrent downloader shared by `scrape` and `find`.
//!
//! - Every file streams to `<name>.part` and is renamed only once complete, so
//!   an interrupted run never leaves a truncated file under the real name.
//! - `--continue` resumes `.part` files with a `Range` request and skips files
//!   that are already fully downloaded (no request at all).
//! - Failed attempts retry with capped exponential backoff; a retry resumes
//!   from the bytes the previous attempt already wrote.
//! - `--delay` spaces request starts across *all* parallel workers.
//! - Each active download gets its own bar with real bytes and bytes/sec.
//! - Every outcome is appended to `<out>/marcopolo.log`.
//! - With scanning on, finished files wait in `<out>/quarantine/` until one
//!   ClamAV run over the batch clears them (see `scan.rs`). Files ClamAV
//!   finds malware in are deleted.

use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use colored::Colorize;
use futures::stream::{self, StreamExt};
use indicatif::{
    HumanBytes, HumanDuration, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle,
};
use reqwest::{header, Client, Response, StatusCode};
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::{
    matches_ext,
    scan::{self, Verdict},
    FileSource, Result,
};

/// Settings shared by every file in one run.
pub struct DownloadOpts {
    /// Files downloaded in parallel.
    pub jobs:     usize,
    /// Resume `.part` files and skip files that already exist.
    pub resume:   bool,
    /// Minimum gap in milliseconds between request starts.
    pub delay_ms: Option<u64>,
    /// Retry attempts per file after the first try.
    pub retries:  u32,
    /// Hide progress and success lines (errors are still printed).
    pub quiet:    bool,
    /// Isolate finished files and scan them with ClamAV before saving.
    pub scan:     bool,
}

/// What a finished run did.
pub struct Summary {
    failed:      usize,
    /// Files ClamAV could not check, held back in `<out>/quarantine/`.
    quarantined: usize,
    /// Files ClamAV found malware in; they were deleted.
    deleted:     usize,
}

impl Summary {
    /// Turns failed, deleted or quarantined files into an error, so the
    /// process exits non-zero and scripts notice.
    pub fn into_result(self) -> Result<()> {
        if self.failed > 0 {
            return Err(format!("{} download(s) failed", self.failed).into());
        }
        if self.deleted > 0 {
            return Err(format!("{} file(s) contained malware and were deleted", self.deleted).into());
        }
        if self.quarantined > 0 {
            return Err(format!("{} file(s) kept in quarantine by the malware scan", self.quarantined).into());
        }
        Ok(())
    }
}

enum Outcome {
    /// Saved; holds the final file size.
    Downloaded(u64),
    /// Already on disk from an earlier `--continue` run.
    Skipped,
    /// Downloaded, but the malware scan kept it in quarantine; holds why.
    Quarantined(String),
    /// ClamAV found malware and the file was deleted; holds the signature name.
    Deleted(String),
}

/// Downloads `files` into `out` and prints/logs the result of each one.
///
/// `gh_client` (which may carry a GitHub token) is used for
/// `raw.githubusercontent.com` so private-repo files work; everything else
/// goes through the plain `client`.
pub async fn download_all(
    client:    &Client,
    gh_client: &Client,
    files:     &[FileSource],
    out:       &Path,
    opts:      &DownloadOpts,
) -> Result<Summary> {
    tokio::fs::create_dir_all(out).await?;

    // The scan needs ClamAV; without it, warn loudly and save files unscanned.
    let clamav = if opts.scan { scan::clamscan_version().await } else { None };
    let scanning = clamav.is_some();
    if opts.scan && !scanning {
        eprintln!(
            "{} ClamAV (clamscan) not found — files will NOT be scanned for malware. \
             Install ClamAV, or pass --no-scan to hide this warning.",
            "warning:".yellow(),
        );
    }
    let quarantine = out.join(scan::QUARANTINE_DIR);
    if scanning {
        tokio::fs::create_dir_all(&quarantine).await?;
    }
    // Where finished files are moved: quarantine until scanned, else `out`.
    let land_in = if scanning { quarantine.as_path() } else { out };

    let mp = if opts.quiet {
        MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
    } else {
        MultiProgress::new()
    };
    let overall = mp.add(ProgressBar::new(files.len() as u64));
    overall.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} files  {elapsed}  ETA {eta}",
        )
        .unwrap()
        .progress_chars("█▓░"),
    );
    overall.enable_steady_tick(Duration::from_millis(120));

    let pacer   = Pacer::new(opts.delay_ms);
    let started = Instant::now();

    let mut results: Vec<(&FileSource, String, Result<Outcome>)> = stream::iter(files)
        .map(|src| {
            let (mp, overall, pacer) = (&mp, &overall, &pacer);
            async move {
                let c = if src.url.starts_with("https://raw.githubusercontent.com/") {
                    gh_client
                } else {
                    client
                };
                let r = download_one(c, src, out, land_in, opts, pacer, mp).await;
                match &r {
                    Ok(Outcome::Downloaded(size)) => {
                        let _ = mp.println(format!(
                            "  {} {}  {}",
                            "✓".green().bold(), src.name, HumanBytes(*size).to_string().dimmed(),
                        ));
                    }
                    Ok(Outcome::Skipped) => {
                        let _ = mp.println(format!(
                            "  {} {}  {}",
                            "·".dimmed(), src.name.dimmed(), "already downloaded".dimmed(),
                        ));
                    }
                    // Only set later, by the scan.
                    Ok(Outcome::Quarantined(_) | Outcome::Deleted(_)) => {}
                    // Errors are shown even in --quiet mode.
                    Err(e) => mp.suspend(|| {
                        eprintln!("  {} {}: {e}", "✗".red().bold(), src.name);
                    }),
                }
                overall.inc(1);
                (src, utc_now(), r)
            }
        })
        .buffer_unordered(opts.jobs.max(1))
        .collect()
        .await;

    overall.finish_and_clear();

    if scanning {
        let clamav = clamav.as_deref().unwrap_or("ClamAV");
        scan_and_release(&mut results, out, &quarantine, clamav, opts.quiet).await;
    }

    let (mut ok, mut skipped, mut failed, mut bytes) = (0usize, 0usize, 0usize, 0u64);
    let (mut quarantined, mut deleted) = (0usize, 0usize);
    let mut log = String::new();
    for (src, stamp, r) in &results {
        let status = match r {
            Ok(Outcome::Downloaded(size)) => { ok += 1; bytes += size; "ok".to_owned() }
            Ok(Outcome::Skipped)          => { skipped += 1; "ok (already downloaded)".to_owned() }
            Ok(Outcome::Quarantined(why)) => { quarantined += 1; format!("quarantined ({why})") }
            Ok(Outcome::Deleted(sig))     => { deleted += 1; format!("deleted (malware {sig}, detected by ClamAV)") }
            Err(e)                        => { failed += 1; format!("error: {e}") }
        };
        let _ = writeln!(log, "[{stamp}] {status}  {}  {}", src.name, src.url);
    }

    let log_path = out.join("marcopolo.log");
    if let Err(e) = append_log(&log_path, &log).await {
        eprintln!("{} could not write log: {e}", "warning:".yellow());
    }

    if !opts.quiet {
        println!(
            "\n{} {} file(s) saved ({} in {}) → {}",
            "✓".green().bold(),
            ok.to_string().green().bold(),
            HumanBytes(bytes),
            HumanDuration(started.elapsed()),
            out.display().to_string().cyan().bold(),
        );
        if skipped > 0 {
            println!("{} {} already downloaded, skipped", "·".dimmed(), skipped);
        }
        if failed > 0 {
            println!("{} {} download(s) failed", "✗".red().bold(), failed.to_string().red());
        }
        if deleted > 0 {
            println!(
                "{} {} file(s) contained malware and were deleted",
                "✗".red().bold(),
                deleted.to_string().red().bold(),
            );
        }
        if quarantined > 0 {
            println!(
                "{} {} file(s) kept in quarantine → {}",
                "⚠".red().bold(),
                quarantined.to_string().red(),
                quarantine.display().to_string().red().bold(),
            );
        }
        println!("{} log → {}", "·".dimmed(), log_path.display().to_string().dimmed());
    }

    Ok(Summary { failed, quarantined, deleted })
}

/// Downloads one file via its `.part` file and moves it into `land_in`
/// (`out`, or the quarantine folder when scanning).
async fn download_one(
    client:  &Client,
    src:     &FileSource,
    out:     &Path,
    land_in: &Path,
    opts:    &DownloadOpts,
    pacer:   &Pacer,
    mp:      &MultiProgress,
) -> Result<Outcome> {
    let dest = out.join(&src.name);
    if opts.resume && tokio::fs::try_exists(&dest).await.unwrap_or(false) {
        return Ok(Outcome::Skipped);
    }
    let part = part_path(&dest);

    let pb = mp.add(ProgressBar::new_spinner());
    pb.set_style(spinner_style());
    pb.set_message(src.name.clone());
    let fetched = fetch_to_part(client, src, &part, opts, pacer, &pb).await;
    pb.finish_and_clear();
    mp.remove(&pb);
    fetched?;

    let saved = land_in.join(&src.name);
    tokio::fs::rename(&part, &saved).await?;
    let size = tokio::fs::metadata(&saved).await.map(|m| m.len()).unwrap_or(0);
    Ok(Outcome::Downloaded(size))
}

/// Scans the quarantine folder with one ClamAV run, then per file:
/// clean → moved into `out`; malware → deleted (`Deleted`); no clear answer
/// → left in quarantine (`Quarantined`), to be safe.
///
/// Prints the scan result: a header, one line per file and the totals.
/// With `quiet`, only deleted and quarantined files are printed.
async fn scan_and_release(
    results:    &mut [(&FileSource, String, Result<Outcome>)],
    out:        &Path,
    quarantine: &Path,
    clamav:     &str,
    quiet:      bool,
) {
    let count = results
        .iter()
        .filter(|(_, _, r)| matches!(r, Ok(Outcome::Downloaded(_))))
        .count();
    if count > 0 {
        let spinner = if quiet { ProgressBar::hidden() } else { ProgressBar::new_spinner() };
        spinner.set_message(format!("scanning {count} file(s) for malware with ClamAV…"));
        spinner.enable_steady_tick(Duration::from_millis(120));
        let started = Instant::now();
        let mut report = scan::scan_dir(quarantine).await;
        spinner.finish_and_clear();

        if !quiet {
            println!(
                "\n{} {} {} file(s) in {}  {}",
                "🛡".cyan().bold(),
                "scan result:".cyan().bold(),
                count,
                HumanDuration(started.elapsed()),
                format!("[{clamav}]").dimmed(),
            );
        }
        let (mut clean, mut infected, mut unscanned) = (0usize, 0usize, 0usize);

        for (src, _, r) in results.iter_mut() {
            if !matches!(r, Ok(Outcome::Downloaded(_))) {
                continue;
            }
            let verdict = match &mut report {
                Ok(verdicts) => verdicts
                    .remove(&src.name)
                    .unwrap_or_else(|| Verdict::Unscanned("no result from clamscan".into())),
                Err(e) => Verdict::Unscanned(format!("clamscan failed: {e}")),
            };
            let in_quarantine = quarantine.join(&src.name);
            match verdict {
                Verdict::Clean => match tokio::fs::rename(&in_quarantine, out.join(&src.name)).await {
                    Ok(()) => {
                        clean += 1;
                        if !quiet {
                            println!("  {} {:<12} {}", "✓".green().bold(), "clean".green(), src.name);
                        }
                    }
                    Err(e) => {
                        unscanned += 1;
                        keep_in_quarantine(r, &src.name, format!("clean, but could not leave quarantine: {e}"));
                    }
                },
                Verdict::Infected(signature) => {
                    infected += 1;
                    match tokio::fs::remove_file(&in_quarantine).await {
                        Ok(()) => {
                            // Shown even in --quiet mode, like errors.
                            eprintln!(
                                "  {} {}",
                                "✗ NOTE:".red().bold(),
                                format!(
                                    "\"{}\" contained malware ({signature}) detected by ClamAV — the file was deleted.",
                                    src.name,
                                ).red().bold(),
                            );
                            *r = Ok(Outcome::Deleted(signature));
                        }
                        Err(e) => keep_in_quarantine(
                            r,
                            &src.name,
                            format!("malware ({signature}) detected by ClamAV, but deleting it failed: {e}"),
                        ),
                    }
                }
                Verdict::Unscanned(reason) => {
                    unscanned += 1;
                    keep_in_quarantine(r, &src.name, format!("not scanned: {reason}"));
                }
            }
        }

        if !quiet {
            let verdict = match (infected, unscanned) {
                (0, 0) => "no threats found".green().bold(),
                (0, _) => "unchecked files kept in quarantine".yellow().bold(),
                (_, _) => "malware found and deleted".red().bold(),
            };
            println!(
                "  {} clean · {} malware · {} not scanned — {verdict}",
                clean.to_string().green(), infected.to_string().red(), unscanned.to_string().yellow(),
            );
        }
    }
    // Succeeds only when the folder is empty, i.e. nothing was left behind.
    let _ = tokio::fs::remove_dir(quarantine).await;
}

/// Marks a file as left in quarantine and says why (shown even in --quiet).
fn keep_in_quarantine(r: &mut Result<Outcome>, name: &str, why: String) {
    eprintln!(
        "  {} {:<12} {}  {}",
        "⚠".yellow().bold(), "quarantined".yellow().bold(), name, format!("— {why}").yellow(),
    );
    *r = Ok(Outcome::Quarantined(why));
}

/// Fetches `src` into `part`, retrying transient failures.
async fn fetch_to_part(
    client: &Client,
    src:    &FileSource,
    part:   &Path,
    opts:   &DownloadOpts,
    pacer:  &Pacer,
    pb:     &ProgressBar,
) -> Result<()> {
    let mut last_err: Option<Box<dyn std::error::Error>> = None;

    for attempt in 0..=opts.retries {
        if attempt > 0 {
            // Saturate + cap so a large --retries can't overflow the backoff.
            let backoff = 500u64.saturating_mul(2u64.saturating_pow(attempt - 1)).min(30_000);
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
        pacer.wait().await;

        // Continue an existing .part file when --continue is set, and always
        // on a retry (those bytes then came from this run's earlier attempt).
        let existing = if opts.resume || attempt > 0 {
            tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let resp = match send_https_only(client, &src.url, existing).await {
            Ok(r)  => r,
            Err(e) => { last_err = Some(e.into()); continue; }
        };
        let status = resp.status();

        // The .part file already holds the whole file.
        if status == StatusCode::RANGE_NOT_SATISFIABLE && existing > 0 {
            return Ok(());
        }
        // 4xx won't change on retry — the link is dead or forbidden.
        if status.is_client_error() {
            return Err(format!("HTTP {status}").into());
        }
        if !status.is_success() {
            last_err = Some(format!("HTTP {status}").into());
            continue;
        }
        // Landing pages, soft-404s and login walls come back as HTML; don't
        // save one under a .pdf/.mp4 name.
        if is_html(&resp) && !matches_ext(&src.name, &["html", "htm"]) {
            return Err("server returned an HTML page, not the file (open the URL in a browser)".into());
        }

        let append = status == StatusCode::PARTIAL_CONTENT && existing > 0;
        if append && !range_starts_at(&resp, existing) {
            // Server answered a different range than asked; start over.
            let _ = tokio::fs::remove_file(part).await;
            last_err = Some("server returned a mismatched byte range".into());
            continue;
        }

        let base = if append { existing } else { 0 };
        match resp.content_length() {
            Some(len) => {
                pb.set_style(bar_style());
                pb.set_length(base + len);
            }
            None => pb.set_style(spinner_style()),
        }
        pb.set_position(base);
        pb.reset_eta();

        match stream_body(resp, part, append, pb).await {
            Ok(())  => return Ok(()),
            Err(e)  => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| "download failed after all retries".into()))
}

/// GETs `url` (from byte `from` when non-zero). The client refuses to follow
/// HTTPS → HTTP redirects; those hops (Gutenberg issues them) are re-requested
/// here over HTTPS so the file never travels over plain HTTP.
async fn send_https_only(client: &Client, url: &str, from: u64) -> reqwest::Result<Response> {
    let mut url = url.to_owned();
    let mut upgrades = 0;
    loop {
        let mut req = client.get(&url);
        if from > 0 {
            req = req.header(header::RANGE, format!("bytes={from}-"));
        }
        let resp = req.send().await?;
        if !resp.status().is_redirection() || upgrades == 5 {
            return Ok(resp);
        }
        let next = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|loc| resp.url().join(loc).ok());
        match next {
            Some(mut next) if next.scheme() == "http" => {
                if next.set_scheme("https").is_err() {
                    return Ok(resp);
                }
                url = next.into();
                upgrades += 1;
            }
            _ => return Ok(resp),
        }
    }
}

/// Streams the body into `path` through a large buffer, so big files never
/// sit in memory and the disk sees few, large writes.
async fn stream_body(mut resp: Response, path: &Path, append: bool, pb: &ProgressBar) -> Result<()> {
    let file = if append {
        tokio::fs::OpenOptions::new().append(true).open(path).await?
    } else {
        tokio::fs::File::create(path).await?
    };
    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    let received = async {
        while let Some(chunk) = resp.chunk().await? {
            writer.write_all(&chunk).await?;
            pb.inc(chunk.len() as u64);
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    // Flush even when the transfer broke off, so a retry can resume from the
    // bytes that did arrive instead of starting over.
    writer.flush().await?;
    received
}

fn is_html(resp: &Response) -> bool {
    resp.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"))
}

/// `true` when a 206 response's `Content-Range` begins at byte `start`.
fn range_starts_at(resp: &Response, start: u64) -> bool {
    resp.headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with(&format!("bytes {start}-")))
}

/// `book.pdf` → `book.pdf.part`
fn part_path(dest: &Path) -> PathBuf {
    let mut name = OsString::from(dest.as_os_str());
    name.push(".part");
    PathBuf::from(name)
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {msg:32!} [{bar:24.cyan/blue}] {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12}  {eta}",
    )
    .unwrap()
    .progress_chars("█▓░")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {msg:32!} {spinner:.cyan} {bytes:>10} {bytes_per_sec:>12}")
        .unwrap()
}

// =============================================================================
// Request pacing (--delay)
// =============================================================================

/// Spaces request starts at least `gap` apart across all workers, so
/// `--delay` is honoured even with several downloads in flight.
struct Pacer {
    gap:  Option<Duration>,
    next: tokio::sync::Mutex<tokio::time::Instant>,
}

impl Pacer {
    fn new(delay_ms: Option<u64>) -> Self {
        Self {
            gap:  delay_ms.filter(|&ms| ms > 0).map(Duration::from_millis),
            next: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        }
    }

    async fn wait(&self) {
        let Some(gap) = self.gap else { return };
        let mut next = self.next.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = tokio::time::Instant::now() + gap;
    }
}

// =============================================================================
// Log file
// =============================================================================

/// Appends `text` to the log so multiple runs accumulate history.
async fn append_log(path: &Path, text: &str) -> Result<()> {
    if text.is_empty() { return Ok(()); }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(text.as_bytes()).await?;
    Ok(())
}

/// Returns the current UTC time formatted as `YYYY-MM-DD HH:MM:SS`.
fn utc_now() -> String {
    // std::time gives us seconds since epoch; format manually to avoid
    // pulling in the `chrono` crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (s, m, h) = (secs % 60, (secs / 60) % 60, (secs / 3600) % 24);
    let days      = secs / 86400; // days since 1970-01-01
    // Quick Gregorian approximation (good enough for log timestamps)
    let (mut y, mut rem) = (1970u32, days);
    loop {
        let dy = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if rem < dy { break; }
        rem -= dy;
        y   += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in = [31u64,if leap{29}else{28},31,30,31,30,31,31,30,31,30,31];
    let (mut mo, mut d) = (1u32, rem + 1);
    for dim in &days_in {
        if d <= *dim { break; }
        d  -= dim;
        mo += 1;
    }
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(part_path(Path::new("out/book.pdf")), PathBuf::from("out/book.pdf.part"));
    }

    #[test]
    fn utc_now_shape() {
        let t = utc_now();
        assert_eq!(t.len(), 19);
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[13..14], ":");
    }
}
