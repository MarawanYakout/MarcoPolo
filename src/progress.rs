//! Download progress kept per output folder, so a run stopped by Ctrl-C, a
//! crash or failed downloads is continued by running the same command again.
//!
//! `<out>/.marcopolo-progress.jsonl` is an append-only journal with one JSON
//! line per event, written the moment it happens so it survives Ctrl-C:
//! - `planned`   — the file was picked for download;
//! - `done`      — it finished downloading (with its size);
//! - `duplicate` — it was identical to another file and deleted.
//!
//! A file counts as done only when the journal names the same URL for it
//! *and* the file is still on disk with the recorded size, so a same-named
//! file from another source is never mistaken for it.

use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use colored::Colorize;
use serde::{Deserialize, Serialize};

use crate::{download::part_path, scan::QUARANTINE_DIR, FileSource};

/// Journal file name inside the output folder (hidden: it is bookkeeping).
const JOURNAL: &str = ".marcopolo-progress.jsonl";

// =============================================================================
// Journal events
// =============================================================================

/// One line of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    /// Picked for download in this run.
    Planned { name: String, url: String },
    /// Finished downloading; `size` in bytes.
    Done { name: String, url: String, size: u64 },
    /// Identical to file `of`, so it was deleted.
    Duplicate { name: String, url: String, of: String },
}

impl Event {
    pub fn planned(file: &FileSource) -> Self {
        Self::Planned { name: file.name.clone(), url: file.url.clone() }
    }

    pub fn done(file: &FileSource, size: u64) -> Self {
        Self::Done { name: file.name.clone(), url: file.url.clone(), size }
    }

    pub fn duplicate(file: &FileSource, of: &str) -> Self {
        Self::Duplicate { name: file.name.clone(), url: file.url.clone(), of: of.to_owned() }
    }
}

/// Appends events to the journal as they happen.
pub struct Journal {
    /// `None` when the journal could not be opened: progress is then simply
    /// not kept, downloads still work.
    file: Option<Mutex<File>>,
}

impl Journal {
    /// Opens (or creates) the journal in `out`, which must exist.
    pub fn open(out: &Path) -> Self {
        match File::options().create(true).append(true).open(out.join(JOURNAL)) {
            Ok(file) => Self { file: Some(Mutex::new(file)) },
            Err(e) => {
                eprintln!(
                    "{} could not open the progress file ({e}); an interrupted run will start over.",
                    "warning:".yellow(),
                );
                Self { file: None }
            }
        }
    }

    /// A journal that keeps nothing.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self { file: None }
    }

    /// Appends `events`, one line each. Each call is a single unbuffered
    /// write, so the lines are on disk even if Ctrl-C comes right after.
    /// A failed write only costs resumability, so it is not an error.
    pub fn record(&self, events: &[Event]) {
        let Some(file) = &self.file else { return };
        let mut text = String::new();
        for line in events.iter().filter_map(|e| serde_json::to_string(e).ok()) {
            text.push_str(&line);
            text.push('\n');
        }
        // A plain (std) mutex, held only for this one synchronous write.
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(text.as_bytes());
        }
    }
}

// =============================================================================
// Reading progress back
// =============================================================================

/// Where a file stands according to an earlier run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Saved in the output folder (same URL, same size): nothing to do.
    Saved,
    /// Downloaded, but the malware scan never ran: it waits in quarantine.
    InQuarantine(u64),
    /// Identical to this saved file, so it was deleted: nothing to do.
    Duplicate(String),
    /// Its `.part` file is left from an interrupted download of the same URL.
    Partial,
    /// Nothing to reuse: download it.
    New,
}

impl Status {
    /// `true` when nothing is left to do for the file.
    pub fn is_done(&self) -> bool {
        match self {
            Self::Saved | Self::Duplicate(_) => true,
            Self::InQuarantine(_) | Self::Partial | Self::New => false,
        }
    }
}

/// URL and size of the last finished download under a name.
#[derive(Debug)]
struct DoneAt {
    url:  String,
    size: u64,
}

/// URL of the last copy deleted under a name, and what it copied.
#[derive(Debug)]
struct CopyOf {
    url: String,
    of:  String,
}

/// What the journal says about one file name. Each kind of event is kept on
/// its own, so a later `planned` doesn't hide an earlier `done`.
#[derive(Debug, Default)]
struct Record {
    planned_url: Option<String>,
    done:        Option<DoneAt>,
    copy:        Option<CopyOf>,
}

/// The journal of one output folder, read back.
#[derive(Debug)]
pub struct Progress {
    out:     PathBuf,
    records: HashMap<String, Record>,
}

impl Progress {
    /// Reads the journal in `out`; a missing journal means no progress.
    pub fn load(out: &Path) -> Self {
        let text = std::fs::read_to_string(out.join(JOURNAL)).unwrap_or_default();
        Self::from_journal(out, &text)
    }

    fn from_journal(out: &Path, text: &str) -> Self {
        let mut records: HashMap<String, Record> = HashMap::new();
        // A line cut short by Ctrl-C (or anything else unreadable) is skipped.
        for event in text.lines().filter_map(|line| serde_json::from_str::<Event>(line).ok()) {
            match event {
                Event::Planned { name, url } => records.entry(name).or_default().planned_url = Some(url),
                Event::Done { name, url, size } => records.entry(name).or_default().done = Some(DoneAt { url, size }),
                Event::Duplicate { name, url, of } => records.entry(name).or_default().copy = Some(CopyOf { url, of }),
            }
        }
        Self { out: out.to_owned(), records }
    }

    /// Where `file` stands. Looks at the disk, so a file deleted or replaced
    /// since the journal entry is downloaded again.
    pub fn status(&self, file: &FileSource) -> Status {
        let Some(rec) = self.records.get(&file.name) else { return Status::New };
        let same_url = |url: &str| url == file.url;

        if let Some(done) = rec.done.as_ref().filter(|d| same_url(&d.url)) {
            if file_size(&self.out.join(&file.name)) == Some(done.size) {
                return Status::Saved;
            }
            if file_size(&self.out.join(QUARANTINE_DIR).join(&file.name)) == Some(done.size) {
                return Status::InQuarantine(done.size);
            }
        }
        if let Some(copy) = rec.copy.as_ref().filter(|c| same_url(&c.url)) {
            if self.out.join(&copy.of).is_file() {
                return Status::Duplicate(copy.of.clone());
            }
        }
        let ours = rec.planned_url.as_deref().is_some_and(same_url)
            || rec.done.as_ref().is_some_and(|d| same_url(&d.url));
        if ours && part_path(&self.out.join(&file.name)).is_file() {
            return Status::Partial;
        }
        Status::New
    }

    /// `true` when the last run into this folder picked `file` (same URL).
    pub fn was_planned(&self, file: &FileSource) -> bool {
        self.records
            .get(&file.name)
            .and_then(|r| r.planned_url.as_deref())
            .is_some_and(|url| url == file.url)
    }
}

/// Which files start checked in the picker: when an earlier run into the same
/// folder picked some of them, exactly those — so a rerun continues that
/// choice — otherwise all of them.
pub fn initial_checks(files: &[FileSource], progress: &Progress) -> Vec<bool> {
    let planned: Vec<bool> = files.iter().map(|f| progress.was_planned(f)).collect();
    if planned.contains(&true) { planned } else { vec![true; files.len()] }
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty folder for one test, removed by the test at the end.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marcopolo-progress-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(QUARANTINE_DIR)).unwrap();
        dir
    }

    fn file(name: &str, url: &str) -> FileSource {
        FileSource { name: name.to_owned(), url: url.to_owned() }
    }

    #[test]
    fn journal_round_trip_and_truncated_line() {
        let dir = test_dir("round-trip");
        let book = file("book.pdf", "https://x.org/book.pdf");
        let journal = Journal::open(&dir);
        journal.record(&[Event::planned(&book), Event::done(&book, 4)]);
        drop(journal);
        // A line cut short by Ctrl-C mid-write.
        let mut f = File::options().append(true).open(dir.join(JOURNAL)).unwrap();
        f.write_all(br#"{"event":"done","name":"other.pdf","url":"#).unwrap();

        std::fs::write(dir.join("book.pdf"), "1234").unwrap();
        let progress = Progress::load(&dir);
        assert_eq!(progress.status(&book), Status::Saved);
        assert!(progress.was_planned(&book));
        assert_eq!(progress.status(&file("other.pdf", "https://x.org/other.pdf")), Status::New);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn statuses_follow_the_disk() {
        let dir = test_dir("statuses");
        let url = "https://x.org/f";
        let journal = [
            Event::Done { name: "saved.pdf".into(), url: url.into(), size: 3 },
            Event::Done { name: "waiting.pdf".into(), url: url.into(), size: 3 },
            Event::Done { name: "changed.pdf".into(), url: url.into(), size: 3 },
            Event::Done { name: "gone.pdf".into(), url: url.into(), size: 3 },
            Event::Planned { name: "half.pdf".into(), url: url.into() },
            Event::Planned { name: "other-half.pdf".into(), url: "https://elsewhere.org/f".into() },
            Event::Duplicate { name: "copy.pdf".into(), url: url.into(), of: "saved.pdf".into() },
            Event::Duplicate { name: "lost-copy.pdf".into(), url: url.into(), of: "gone.pdf".into() },
        ]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap() + "\n")
        .collect::<String>();
        std::fs::write(dir.join("saved.pdf"), "abc").unwrap();
        std::fs::write(dir.join(QUARANTINE_DIR).join("waiting.pdf"), "abc").unwrap();
        std::fs::write(dir.join("changed.pdf"), "abcdef").unwrap();
        std::fs::write(dir.join("half.pdf.part"), "a").unwrap();
        std::fs::write(dir.join("other-half.pdf.part"), "a").unwrap();

        let p = Progress::from_journal(&dir, &journal);
        let status = |name: &str| p.status(&file(name, url));
        assert_eq!(status("saved.pdf"), Status::Saved);
        assert_eq!(status("waiting.pdf"), Status::InQuarantine(3));
        assert_eq!(status("changed.pdf"), Status::New, "replaced since: download again");
        assert_eq!(status("gone.pdf"), Status::New, "deleted since: download again");
        assert_eq!(status("half.pdf"), Status::Partial);
        assert_eq!(status("other-half.pdf"), Status::New, ".part of another URL is not ours");
        assert_eq!(status("copy.pdf"), Status::Duplicate("saved.pdf".into()));
        assert_eq!(status("lost-copy.pdf"), Status::New, "the original is gone: download again");
        assert_eq!(status("never-seen.pdf"), Status::New);
        // Same name, different source: never mistaken for the saved file.
        assert_eq!(p.status(&file("saved.pdf", "https://elsewhere.org/f")), Status::New);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn later_planned_keeps_earlier_done() {
        let dir = test_dir("planned-after-done");
        let book = file("book.pdf", "https://x.org/book.pdf");
        let journal = [Event::done(&book, 3), Event::planned(&book)]
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect::<String>();
        std::fs::write(dir.join("book.pdf"), "abc").unwrap();
        assert_eq!(Progress::from_journal(&dir, &journal).status(&book), Status::Saved);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn picker_checks_continue_the_last_choice() {
        let dir = test_dir("checks");
        let (a, b, c) = (file("a.pdf", "u/a"), file("b.pdf", "u/b"), file("c.pdf", "u/c"));
        let files = [a.clone(), b.clone(), c.clone()];

        // No earlier run: everything checked.
        assert_eq!(initial_checks(&files, &Progress::from_journal(&dir, "")), [true, true, true]);

        // Last time only a and c were picked.
        let journal = [Event::planned(&a), Event::planned(&c)]
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect::<String>();
        assert_eq!(initial_checks(&files, &Progress::from_journal(&dir, &journal)), [true, false, true]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
