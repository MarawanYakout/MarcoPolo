<div align="center">

<img src="logo.png" width="400" />

# 🗺️ Marcopolo

<img src="https://img.shields.io/badge/version-0.3.0-4f98a3?style=for-the-badge&logoColor=white" />
<img src="https://img.shields.io/badge/rust-2021_edition-orange?style=for-the-badge&logo=rust&logoColor=white" />
<img src="https://img.shields.io/badge/license-MIT-success?style=for-the-badge" />
<img src="https://img.shields.io/badge/async-tokio-blue?style=for-the-badge&logo=tokio&logoColor=white" />

**Find Any Book, Scrape any URL. Grab Any File Type, Any Format. One command.**

</div>

A fast command-line tool that hunts and downloads files from GitHub repositories
and websites. Supports PDFs, text documents, images, and videos.

---

## Installation

**Requirements:** Rust 1.70+ — install via [rustup.rs](https://rustup.rs)

```bash
git clone https://github.com/yourname/marcopolo
cd marcopolo
cargo install --path .
```

Verify it works:

```bash
marcopolo --version
# marcopolo 0.3.0
```

**Recommended:** install the free ClamAV antivirus as well, so every download
is checked for malware. It is a separate program, installed with your own
package manager — see [Malware Scan (ClamAV)](#malware-scan-clamav).

---

##  Usage

Just point marcopolo at any URL and it handles the rest — no subcommand
needed, a link on its own is scraped:

```bash
marcopolo https://github.com/owner/repo
# same as: marcopolo scrape https://github.com/owner/repo
```

That's it. It finds the files, shows them in a checkbox list so you choose
what to download, then checks each download for malware with ClamAV.

---

## Usage

Want to target specific file types? Use `--type` (default: `pdf`; repeat it
to grab several, e.g. `--type pdf --type img`):

| Flag | What it grabs |
|------|--------------|
| `--type pdf` | PDF documents (pdf) |
| `--type text` | Text & document files (txt, md, epub, doc, docx, csv, rst) |
| `--type img` | Images (jpg, jpeg, png, gif, svg, webp, bmp, ico) |
| `--type video` | Video files (mp4, mkv, avi, mov, webm, flv, m4v) |

**Planned** — these types are not accepted yet and will be added in a future
version (using them today gives an error):

| Flag | What it will grab |
|------|--------------|
| `--type audio` | Audio files (mp3, wav, flac, ogg) |
| `--type zip` | Archives (zip, tar, gz, rar, 7z) |
| `--type code` | Source files (rs, py, js, ts, go, cpp) |
| `--type data` | Data files (json, xml, yaml) |
| `--type all` | Everything |

### Additional flags

| Flag | Description |
|------|-------------|
| `--list` | Preview files without downloading |
| `--depth <n>` | How many links deep to crawl (default: 1) |
| `--out <dir>` | Output directory (default: `./downloads`) |

---


### How It Works

Marcopolo operates in two modes depending on the URL you give it:

**GitHub mode** — when the URL is on `github.com`:
1. Scans the full repository tree for committed files
2. Reads every README and Markdown/reST doc in the repo (not just the root
   README) and extracts all file links (`/blob/` links are rewritten to the
   raw file) — reading-list repos keep their PDF links in nested docs
3. Scans all GitHub Release assets

**Web mode** — for any other URL:
1. Checks `/sitemap.xml` at the root domain first (fast path)
2. BFS-crawls from the landing page up to `--depth` levels deep
3. Only follows same-origin links — never crawls external sites

All discovered files are deduplicated by URL, then downloaded concurrently
(4 at a time by default, see `--jobs`) into `./downloads/`. Files with the same
name get `_1`, `_2`, … suffixes instead of overwriting each other.

**Choose before downloading** — in a terminal, marcopolo shows every
discovered file in a checkbox list, all checked. Untick what you don't want,
then press `d`:

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | Move (`PgUp` / `PgDn` jump a page) |
| `space` | Check / uncheck |
| `a` | Check all / none |
| `enter` | Show / hide the full link (links are shortened to `host/…/file.pdf`) |
| `d` | Download the checked files |
| `q` / `esc` | Quit without downloading |

With `--quiet`, or when output is piped (scripts, cron), there is no list and
everything is downloaded as before.

Each file streams to `<name>.part` and is renamed only when complete, so an
interrupted run never leaves a truncated file behind under the real name.

**Malware scan** — every download is checked with ClamAV before it is
saved; files with malware are deleted. See
[Malware Scan (ClamAV)](#malware-scan-clamav).

---


### Options

| Flag | Default | Description |
|---|---|---|
| `--type`, `-t` | `pdf` | File type(s) to hunt. Repeatable. |
| `--out`, `-o` | `downloads` | Output directory |
| `--depth` | `1` | BFS crawl depth (web mode only) |
| `--delay` | none | Minimum milliseconds between requests (across all jobs) |
| `--continue` | off | Resume `.part` files, skip files already downloaded |
| `--retries` | `3` | Retry attempts on failure |
| `--jobs`, `-j` | `4` | Files downloaded in parallel (1–64) |
| `--list` | off | Dry run — list files without downloading |
| `--filter` | none | Only download files matching this keyword |
| `--quiet` | off | Suppress all output except errors |
| `--no-scan` | off | Skip the ClamAV malware scan (also on `find`) |

A GitHub token is read from the `GITHUB_TOKEN` (or `MARCOPOLO_GITHUB_TOKEN`)
environment variable — see [Use a GitHub token](#use-a-github-token-recommended).

### Supported file types

| Flag value | Extensions |
|---|---|
| `pdf` | `.pdf` |
| `text` | `.txt` `.md` `.epub` `.doc` `.docx` `.csv` `.rst` |
| `img` | `.jpg` `.jpeg` `.png` `.gif` `.svg` `.webp` `.bmp` `.ico` |
| `video` | `.mp4` `.mkv` `.avi` `.mov` `.webm` `.flv` `.m4v` |

---

## Malware Scan (ClamAV)

marcopolo checks every file it downloads with
[ClamAV](https://www.clamav.net), the free open-source antivirus, so a
poisoned PDF never lands in your folder.

**How it works**

1. Each finished download goes into an isolated folder, `<out>/quarantine/`,
   instead of straight into `<out>/`.
2. When the batch is done, ClamAV scans the whole folder in one run (about
   7 seconds — that is ClamAV loading its virus signatures).
3. Each file then gets one of three results:
   - **clean** → moved into `<out>/`.
   - **malware** → **deleted**, with a red note naming the file and what
     ClamAV found.
   - **could not be checked** (e.g. ClamAV's signatures failed to load) →
     left in `<out>/quarantine/` for you to decide.
4. A scan result is printed, every outcome is written to `marcopolo.log`, and
   marcopolo exits with code 1 if anything was deleted or left in quarantine.

### Install ClamAV

ClamAV is **not** bundled with marcopolo — install it separately with your
system's package manager. marcopolo finds it automatically (it runs
`clamscan` from your `PATH`).

| System | Install command |
|---|---|
| Arch / Manjaro / EndeavourOS | `sudo pacman -S clamav` *(official `extra` repo — no AUR needed; `yay -S clamav` / `paru -S clamav` also work)* |
| Debian / Ubuntu / Mint | `sudo apt install clamav clamav-freshclam` |
| Fedora / RHEL | `sudo dnf install clamav clamav-update` |
| openSUSE | `sudo zypper install clamav` |
| macOS | `brew install clamav` *(then see the note below)* |
| Windows | installer from [clamav.net/downloads](https://www.clamav.net/downloads) |

**Download the virus signatures** (required before the first scan, and
whenever they get old):

```bash
sudo freshclam
```

**Keep them updated automatically** (recommended — otherwise new malware is
missed):

```bash
# Arch, Fedora
sudo systemctl enable --now clamav-freshclam
# Debian / Ubuntu: the clamav-freshclam service already runs after install
# (a manual `sudo freshclam` may then report a locked log file — that's fine)
```

**macOS (Homebrew)** needs a config file before `freshclam` runs:

```bash
cp "$(brew --prefix)/etc/clamav/freshclam.conf.sample" "$(brew --prefix)/etc/clamav/freshclam.conf"
sed -i '' '/^Example/d' "$(brew --prefix)/etc/clamav/freshclam.conf"
freshclam
```

**Check it works:**

```bash
clamscan --version
# ClamAV 1.5.4/28158/Mon Oct 19 09:26:01 2026   ← engine / signature version / signature date
```

The date at the end is when your signatures were last updated. marcopolo
shows it in every scan result, so you can see if they are getting old.

### Example 1 — a good file

```bash
marcopolo https://github.com/MarawanYakout/MarcoPolo --type img
```

```text
🛡 scan result: 1 file(s) in 7 seconds  [ClamAV 1.5.4, signatures v28158 (Mon Oct 19 09:26:01 2026)]
  ✓ clean        logo.png
  1 clean · 0 malware · 0 not scanned — no threats found

✓ 1 file(s) saved (37.49 KiB in 9 seconds) → downloads
```

ClamAV found nothing, so `logo.png` is moved from `downloads/quarantine/` to
`downloads/`, the empty quarantine folder is removed, and marcopolo exits
with code 0.

### Example 2 — a bad file

This downloads the [EICAR test file](https://www.eicar.org/download-anti-malware-testfile/):
a harmless text string that every antivirus treats as malware, made for
safely testing scanners.

```bash
marcopolo https://www.eicar.org/download-anti-malware-testfile/ --type text --depth 0
```

```text
🛡 scan result: 1 file(s) in 7 seconds  [ClamAV 1.5.4, signatures v28158 (Mon Oct 19 09:26:01 2026)]
  ✗ NOTE: "eicar.com.txt" contained malware (Eicar-Test-Signature) detected by ClamAV — the file was deleted.
  0 clean · 1 malware · 0 not scanned — malware found and deleted

✓ 0 file(s) saved (0 B in 7 seconds) → downloads
✗ 1 file(s) contained malware and were deleted
error: 1 file(s) contained malware and were deleted
```

The file never leaves quarantine: it is deleted on the spot (the red note is
shown even with `--quiet`), `marcopolo.log` records
`deleted (malware Eicar-Test-Signature, detected by ClamAV)`, and marcopolo
exits with code 1 so scripts notice.

### Without ClamAV, or turning it off

- **ClamAV not installed** → a yellow warning says files are **not** scanned,
  and downloads are saved as before.
- **`--no-scan`** → skip the scan on purpose (works with `scrape` and `find`).
- **Large files** → by default ClamAV only checks the first 100 MB of a file,
  so very large videos are not fully scanned. PDFs are well under this.

---

## Examples

### Download PDFs from a GitHub repo (default)

```bash
marcopolo https://github.com/varunkashyapks/Books
```

Downloads all `.pdf` files committed in the repo into `./downloads/`.

---

### Download PDFs linked inside a README

```bash
marcopolo "https://github.com/Carl-McBride-Ellis/Compendium-of-free-ML-reading-resources?tab=readme-ov-file"
```

marcopolo reads the README and every other doc in the repo, finds all `https://...pdf` links, and lets you pick which to download.

---

### Download PDFs from a website

```bash
marcopolo https://themlbook.com/wiki/doku.php
```

Scrapes the page and one level of internal links for PDF hrefs.

---

### Crawl deeper into a website

```bash
marcopolo https://somesite.com/resources --depth 3
```

Follows links up to 3 levels deep from the landing page.

---

### Download images instead of PDFs

```bash
marcopolo https://github.com/owner/repo --type img
```

---

### Download multiple file types at once

```bash
marcopolo https://github.com/owner/repo --type pdf --type text
marcopolo https://somesite.com --type pdf --type img --type video
```

---

### Preview what would be downloaded (dry run)

```bash
marcopolo https://github.com/owner/repo --list
```

Prints the filename and URL of every discovered file. Nothing is downloaded.


---

### Filter by keyword

```bash
marcopolo https://github.com/owner/repo --filter "transformer"
```

Only downloads files whose filename contains `transformer` (case-insensitive).

Combine with `--list` to preview the filtered results first:

```bash
marcopolo https://github.com/owner/repo --filter "transformer" --list
```

---

### Save to a custom folder

```bash
marcopolo https://github.com/owner/repo --out ~/papers
marcopolo https://somesite.com --out ./my-downloads/site-files
```

The folder is created automatically if it does not exist.

---

### Use a GitHub token (recommended)

Without a token, GitHub allows 60 API requests per hour.
With a token, the limit raises to 5,000 per hour.

Generate one at: [github.com/settings/tokens](https://github.com/settings/tokens)
No scopes needed for public repos. Add `repo` scope for private repos.

```bash
export GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
marcopolo https://github.com/owner/repo
```

The token is read from the environment (never a CLI flag, which would be
visible to other users via `ps`) and is only sent to GitHub hosts.

---

### Resume an interrupted download

```bash
marcopolo https://github.com/owner/repo --continue
```

Files that are already fully downloaded are skipped without a request, and
`.part` files are continued with a `Range` request instead of restarting
from zero. Retries within a run always resume from the bytes already received.

---

### Add a delay between requests (be polite)

```bash
marcopolo https://somesite.com --delay 500
```

Spaces request starts at least 500ms apart, even with several parallel
jobs. Useful for sites that rate-limit scrapers.

---

### Download faster

```bash
marcopolo https://github.com/owner/repo --jobs 16
```

Raises parallel downloads from 4 to 16 (max 64). Each active download shows
its own progress bar with real bytes and speed.

---

### Change retry attempts

```bash
marcopolo https://github.com/owner/repo --retries 5
```

Retries failed downloads up to 5 times with exponential back-off (500ms, 1s, 2s…).
`4xx` errors (404, 403) are never retried — the link is simply dead.

---

## Real-World Recipes

### Grab every ML paper PDF from a curated list

```bash
export GITHUB_TOKEN=ghp_xxx
marcopolo "https://github.com/Carl-McBride-Ellis/Compendium-of-free-ML-reading-resources" \
  --out ~/ml-papers \
  --retries 5 \
  --delay 200
```

---

### Download an entire book repo

```bash
marcopolo https://github.com/varunkashyapks/Books \
  --type pdf \
  --out ~/books \
  --continue
```

---

### Scrape a documentation site for all text files

```bash
marcopolo https://docs.someproject.org \
  --type text \
  --depth 2 \
  --delay 300 \
  --out ./docs-backup
```

---

### Download only images from a GitHub repo, filtered by name

```bash
marcopolo https://github.com/owner/design-assets \
  --type img \
  --filter "logo" \
  --out ./logos
```

---

### Check what a site has before committing to a download

```bash
# Step 1 — see what's there
marcopolo https://somesite.com/resources --type pdf --depth 2 --list

# Step 2 — download only what you want
marcopolo https://somesite.com/resources --type pdf --depth 2 --filter "2024"
```

---

##  Find Mode (New) - Finding Books at important parts of the internet

Marcopolo now includes a powerful `find` command to search for free books and documents across the web's largest open libraries.

### Search Sources
When you use `find`, Marcopolo queries these sources in parallel:
*   **Internet Archive** (archive.org) — Reliable JSON API
*   **Open Library** (openlibrary.org) — Curated metadata
*   **Project Gutenberg** (gutenberg.org) — Via Gutendex API
*   **Anna's Archive** (annas-archive.org) — HTML scraping; often blocked by a bot check, in which case it simply returns no results
*   **Github** (Github.com) — Searches PDFs with similar names on github
*   **Googlescholar** (Googlescholar.com) — Searches PDFs / Books / Articles with similar names on Googlescholar
*   **Duckduckgo** (Duckduckgo.com) — Searches PDFs with similar names using Duckduckgo 0% data collection

Results from all sources are ranked by how well their title matches your
query; results sharing no word with it are dropped. A source that hasn't
answered within 12 seconds is skipped, so one slow site can't stall the search.
Archive.org results only include publicly downloadable items (lend-only books
are excluded), and the real PDF inside each item is looked up before download.




### Usage Examples

**Search and pick results (default):**
```bash
marcopolo find "Clean Code"
```
Results open in the same checkbox list as `scrape` (none checked): one line
per result with a shortened link — press `enter` on a result to see its full
link, `space` to check it, `d` to download.

**Download the top match from each source:**
```bash
marcopolo find "The Pragmatic Programmer" --get --out ~/my-books
```

**Restrict search to a specific source:**
```bash
marcopolo find "Computer Systems" --source archive
```
*Options for `--source`: `archive`, `openlibrary`, `gutenberg`, `annas`, `github`, `googlescholar`, `duckduckgo`*

**List results for a specific query without downloading:**
```bash
marcopolo find "Rust Programming" --list
```

**Only show one format (`pdf`, `epub`, `txt`, `mobi`, `djvu`):**
```bash
marcopolo find "Pride and Prejudice" --format txt --get
```

### Advanced PDF Finding
Marcopolo now includes specialized sources that excel at finding PDFs across the broader internet:

**Search GitHub repositories for committed PDFs:**
```bash
marcopolo find "machine learning" --source github --list
```

**Search Google Scholar for academic papers:**
```bash
marcopolo find "transformer networks" --source googlescholar --list
```

**Search DuckDuckGo specifically for PDF files:**
```bash
marcopolo find "rustlang" --source duckduckgo --list
```

---

## Understanding Download Errors

These are printed during a run and are **normal**. They do not crash marcopolo.

| Error | Meaning |
|---|---|
| `404 Not Found` | The file was moved or deleted on the remote server |
| `403 Forbidden` | The server blocks direct downloads |
| `500 Domain Not Found` | The website no longer exists |
| `error sending request` | The server is unreachable or timed out |
| `server returned an HTML page, not the file` | The link leads to a web page (login wall, landing page, soft 404) rather than the file |

Every file that *can* be downloaded will be. Dead links are skipped and reported,
every outcome is appended to `<out>/marcopolo.log`, and marcopolo exits with
status `1` if any download failed, so scripts can detect partial failures.

Check what was successfully saved:

```bash
ls -lh downloads/
ls downloads/ | wc -l
```

---

## Updating

After pulling new code from the same clone link or making changes:

```bash
cargo install --path .
```

Use inside marcopolo's folder, this rebuilds in release mode and overwrites the global binary automatically.

---


