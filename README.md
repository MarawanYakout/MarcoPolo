<div align="center">

# 🗺️ Marcopolo

<img src="https://img.shields.io/badge/version-0.3.0-4f98a3?style=for-the-badge&logoColor=white" />
<img src="https://img.shields.io/badge/rust-2021_edition-orange?style=for-the-badge&logo=rust&logoColor=white" />
<img src="https://img.shields.io/badge/license-MIT-success?style=for-the-badge" />
<img src="https://img.shields.io/badge/async-tokio-blue?style=for-the-badge&logo=tokio&logoColor=white" />
<img src="https://img.shields.io/badge/built_with-❤️-red?style=for-the-badge" />

**Scrape any URL. Grab any file. One command.**

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

---

##  Usage

Just point marcopolo at any URL and it handles the rest:

```bash
marcopolo https://github.com/owner/repo
```

That's it. It crawls the page and downloads everything it finds.

---

## Usage

Want to target specific file types? Use `--type`:

| Flag | What it grabs |
|------|--------------|
| `--type pdf` | PDF documents |
| `--type img` | Images (png, jpg, gif, webp, svg) |
| `--type video` | Video files (mp4, mkv, avi, mov) |
| `--type audio` | Audio files (mp3, wav, flac, ogg) |
| `--type zip` | Archives (zip, tar, gz, rar, 7z) |
| `--type doc` | Word / text docs (docx, doc, txt, odt) |
| `--type code` | Source files (rs, py, js, ts, go, cpp) |
| `--type data` | Data files (csv, json, xml, yaml) |
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

**GitHub mode** — when the URL contains `github.com`:
1. Scans the full repository tree for committed files
2. Decodes the README and extracts all hyperlinks
3. Scans all GitHub Release assets
4. All three run in parallel

**Web mode** — for any other URL:
1. Checks `/sitemap.xml` at the root domain first (fast path)
2. BFS-crawls from the landing page up to `--depth` levels deep
3. Only follows same-origin links — never crawls external sites

All discovered files are deduplicated by URL, then downloaded concurrently
(4 at a time by default) into `./downloads/`.

---


### Options

| Flag | Default | Description |
|---|---|---|
| `--type`, `-t` | `pdf` | File type(s) to hunt. Repeatable. |
| `--out`, `-o` | `downloads` | Output directory |
| `--depth` | `1` | BFS crawl depth (web mode only) |
| `--delay` | none | Milliseconds between downloads |
| `--continue` | off | Resume partially downloaded files |
| `--retries` | `3` | Retry attempts on failure |
| `--list` | off | Dry run — list files without downloading |
| `--filter` | none | Only download files matching this keyword |
| `--token` | none | GitHub personal access token |

### Supported file types

| Flag value | Extensions |
|---|---|
| `pdf` | `.pdf` |
| `text` | `.txt` `.md` `.epub` `.doc` `.docx` `.csv` `.rst` |
| `img` | `.jpg` `.jpeg` `.png` `.gif` `.svg` `.webp` `.bmp` `.ico` |
| `video` | `.mp4` `.mkv` `.avi` `.mov` `.webm` `.flv` `.m4v` |

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

marcopolo decodes the README, finds all `https://...pdf` links, and downloads them.

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
marcopolo https://github.com/owner/repo --token ghp_xxxxxxxxxxxxxxxxxxxx
```

---

### Resume an interrupted download

```bash
marcopolo https://github.com/owner/repo --continue
```

Sends a `Range` header and appends bytes to partially downloaded files
instead of restarting from zero.

---

### Add a delay between requests (be polite)

```bash
marcopolo https://somesite.com --delay 500
```

Waits 500ms before each download. Useful for sites that rate-limit scrapers.

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
marcopolo "https://github.com/Carl-McBride-Ellis/Compendium-of-free-ML-reading-resources" \
  --token ghp_xxx \
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

## Understanding Download Errors

These are printed during a run and are **normal**. They do not crash marcopolo.

| Error | Meaning |
|---|---|
| `404 Not Found` | The file was moved or deleted on the remote server |
| `403 Forbidden` | The server blocks direct downloads |
| `500 Domain Not Found` | The website no longer exists |
| `error sending request` | The server is unreachable or timed out |

Every file that *can* be downloaded will be. Dead links are skipped and reported.

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


