//! Malware check for downloaded files, using the ClamAV scanner installed on
//! this machine (`clamscan`, <https://www.clamav.net>).
//!
//! How `download.rs` uses it:
//! 1. Every finished download is moved into `<out>/quarantine/`, not `<out>/`.
//! 2. After the whole batch, [`scan_dir`] runs `clamscan` **once** over that
//!    folder. Loading ClamAV's signature database takes several seconds, so
//!    one run per batch is far faster than one run per file.
//! 3. Files ClamAV calls clean move on to `<out>/`; infected files, and files
//!    ClamAV could not judge, stay in quarantine.

use std::{collections::HashMap, io, path::Path};

use tokio::process::Command;

/// Folder inside the output directory where files wait for their scan, and
/// where files that do not pass it stay.
pub const QUARANTINE_DIR: &str = "quarantine";

/// ClamAV's answer for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// ClamAV found nothing (`OK`, or `Empty file`).
    Clean,
    /// ClamAV matched a signature; holds its name, e.g. `Eicar-Test-Signature`.
    Infected(String),
    /// ClamAV could not judge the file (e.g. a read error); holds its message.
    Unscanned(String),
}

/// Reads the result part of one `clamscan` line (`OK`, `<signature> FOUND`, …).
impl From<&str> for Verdict {
    fn from(result: &str) -> Self {
        if result == "OK" || result == "Empty file" {
            Self::Clean
        } else if let Some(signature) = result.strip_suffix(" FOUND") {
            Self::Infected(signature.to_owned())
        } else {
            Self::Unscanned(result.to_owned())
        }
    }
}

/// ClamAV's version line, or `None` when `clamscan` can't be started.
/// Cheap: `--version` does not load the signature database.
pub async fn clamscan_version() -> Option<String> {
    let output = Command::new("clamscan").arg("--version").output().await.ok()?;
    output
        .status
        .success()
        .then(|| describe_version(String::from_utf8_lossy(&output.stdout).trim()))
}

/// `ClamAV 1.5.4/28158/Mon Oct 19 09:26:01 2026`
/// → `ClamAV 1.5.4, signatures v28158 (Mon Oct 19 09:26:01 2026)`,
/// so the age of the signatures is easy to see. Other shapes pass through.
fn describe_version(raw: &str) -> String {
    match raw.splitn(3, '/').collect::<Vec<_>>().as_slice() {
        [engine, db, date] => format!("{engine}, signatures v{db} ({date})"),
        _ => raw.to_owned(),
    }
}

/// Runs one `clamscan` over the files directly inside `dir` (not subfolders)
/// and returns each file name's verdict. Files ClamAV printed nothing about
/// are missing from the map; callers must treat those as not scanned.
pub async fn scan_dir(dir: &Path) -> io::Result<HashMap<String, Verdict>> {
    // tokio::process runs clamscan without blocking the async runtime.
    // `--no-summary` drops the totals block, so stdout is one line per file;
    // `--` stops a folder named like `-x` from being read as a flag.
    let output = Command::new("clamscan")
        .arg("--no-summary")
        .arg("--")
        .arg(dir)
        .output()
        .await?;

    let verdicts = parse_report(&String::from_utf8_lossy(&output.stdout));
    // clamscan exits 0 (clean), 1 (virus found) or 2 (error). A failure with
    // no per-file lines (e.g. a missing signature database) gives us nothing
    // to trust, so surface ClamAV's last error message instead.
    if verdicts.is_empty() && !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("clamscan failed");
        return Err(io::Error::other(reason.to_owned()));
    }
    Ok(verdicts)
}

/// Parses `clamscan` lines such as `dir/book.pdf: OK` or
/// `dir/book.pdf: Eicar-Test-Signature FOUND` into `file name → verdict`.
fn parse_report(stdout: &str) -> HashMap<String, Verdict> {
    stdout
        .lines()
        .filter_map(|line| {
            // Split on the *last* ": " — file names may contain ": ",
            // ClamAV's results never do.
            let (path, result) = line.rsplit_once(": ")?;
            let name = Path::new(path).file_name()?.to_str()?.to_owned();
            Some((name, Verdict::from(result)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_result_kind() {
        let report = parse_report(
            "out/quarantine/book.pdf: OK\n\
             out/quarantine/empty.pdf: Empty file\n\
             out/quarantine/bad.pdf: Eicar-Test-Signature FOUND\n\
             out/quarantine/locked.pdf: Can't open file or directory ERROR\n",
        );
        assert_eq!(report.len(), 4);
        assert_eq!(report["book.pdf"], Verdict::Clean);
        assert_eq!(report["empty.pdf"], Verdict::Clean);
        assert_eq!(report["bad.pdf"], Verdict::Infected("Eicar-Test-Signature".into()));
        assert_eq!(
            report["locked.pdf"],
            Verdict::Unscanned("Can't open file or directory ERROR".into()),
        );
    }

    #[test]
    fn file_name_containing_colon_space() {
        let report = parse_report("q/Part 1: Intro.pdf: OK\n");
        assert_eq!(report["Part 1: Intro.pdf"], Verdict::Clean);
    }

    #[test]
    fn ignores_lines_that_are_not_results() {
        assert!(parse_report("\n----------- SCAN SUMMARY -----------\n").is_empty());
    }

    #[test]
    fn version_shows_signature_date() {
        assert_eq!(
            describe_version("ClamAV 1.5.4/28158/Mon Oct 19 09:26:01 2026"),
            "ClamAV 1.5.4, signatures v28158 (Mon Oct 19 09:26:01 2026)",
        );
        assert_eq!(describe_version("ClamAV 1.5.4"), "ClamAV 1.5.4");
    }

    #[test]
    fn found_without_signature_name_is_still_infected() {
        // Must never fall through to Clean.
        assert_eq!(Verdict::from(" FOUND"), Verdict::Infected(String::new()));
        assert_eq!(Verdict::from("FOUND"), Verdict::Unscanned("FOUND".into()));
    }

    /// Runs the real `clamscan` (≈7 s to load signatures), so it is opt-in:
    /// `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "needs ClamAV installed; slow"]
    async fn real_clamscan_flags_eicar() {
        let dir = std::env::temp_dir().join(format!("marcopolo-scan-test-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // EICAR: the harmless standard antivirus test string. Built from two
        // halves so this source file itself never matches the signature.
        let eicar = [r"X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDA", "RD-ANTIVIRUS-TEST-FILE!$H+H*"].concat();
        tokio::fs::write(dir.join("bad.pdf"), eicar).await.unwrap();
        tokio::fs::write(dir.join("good.pdf"), "%PDF-1.4 hello").await.unwrap();

        let report = scan_dir(&dir).await;
        tokio::fs::remove_dir_all(&dir).await.unwrap();
        let report = report.unwrap();

        assert!(matches!(report["bad.pdf"], Verdict::Infected(_)));
        assert_eq!(report["good.pdf"], Verdict::Clean);
    }
}
