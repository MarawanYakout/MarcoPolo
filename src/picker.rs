//! Checkbox list for choosing what to download, drawn in the terminal with the
//! `console` crate (already a dependency of `indicatif`, so the build gains
//! no new crate).
//!
//! Keys: ↑/↓ or k/j move · PgUp/PgDn page · Space check · a check all/none ·
//! Enter show/hide the full link · d download checked · q/Esc/Ctrl-C cancel.
//!
//! Links are shortened to `host/…/file.pdf` so every entry fits on one line;
//! Enter expands the entry under the cursor to its full link.
//!
//! [`ListState`] and [`render`] are plain data → text, so they are unit-tested
//! without a terminal; only [`pick`] touches the screen and keyboard.

use std::{
    borrow::Cow,
    io::{self, IsTerminal},
};

use console::{style, truncate_str, Key, Term};
use percent_encoding::percent_decode_str;

/// One entry in the list.
pub struct Item<'a> {
    /// Main text, e.g. a file name or `(archive.org) Clean Code [PDF]`.
    pub label: Cow<'a, str>,
    /// Full link; shown shortened until expanded.
    pub link:  &'a str,
}

/// `true` when both stdin and stdout are a real terminal, i.e. a person is
/// there to answer. Pipes, cron jobs and CI keep the non-interactive path.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Shows `items` as a checkbox list and waits for the user. `checked` says
/// which items start checked, one per item (missing ones start unchecked).
///
/// Returns the checked indices in list order, or `None` when cancelled.
pub fn pick(title: &str, items: &[Item], mut checked: Vec<bool>) -> io::Result<Option<Vec<usize>>> {
    if items.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let term = Term::stdout();
    let _cursor = HiddenCursor::new(&term)?;
    checked.resize(items.len(), false);
    let mut state = ListState::new(checked);
    let mut drawn = 0;

    loop {
        // Re-read the size every frame so a resized window redraws correctly.
        let (rows, cols) = term.size();
        let screen = Screen { rows: rows.into(), cols: cols.into() };
        let list_rows = list_rows(&state, items, screen);
        state.scroll_into_view(list_rows);

        let lines = render(&state, title, items, screen, list_rows);
        clear(&term, drawn)?;
        for line in &lines {
            term.write_line(line)?;
        }
        drawn = lines.len();

        // `read_key_raw` reports Ctrl-C as a key instead of killing the
        // process, so the cursor is always restored.
        let Some(action) = action_for(&term.read_key_raw()?) else { continue };
        match state.apply(action, list_rows) {
            Step::Stay => {}
            Step::Confirmed(chosen) => {
                clear(&term, drawn)?;
                return Ok(Some(chosen));
            }
            Step::Cancelled => {
                clear(&term, drawn)?;
                return Ok(None);
            }
        }
    }
}

// =============================================================================
// State
// =============================================================================

/// What a key press asks the list to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Toggle,
    ToggleAll,
    Expand,
    Confirm,
    Cancel,
}

/// Maps a key to its action; other keys do nothing.
fn action_for(key: &Key) -> Option<Action> {
    match key {
        Key::ArrowUp | Key::Char('k')                  => Some(Action::Up),
        Key::ArrowDown | Key::Char('j')                => Some(Action::Down),
        Key::PageUp                                    => Some(Action::PageUp),
        Key::PageDown                                  => Some(Action::PageDown),
        Key::Char(' ')                                 => Some(Action::Toggle),
        Key::Char('a')                                 => Some(Action::ToggleAll),
        Key::Enter                                     => Some(Action::Expand),
        Key::Char('d')                                 => Some(Action::Confirm),
        Key::Escape | Key::Char('q') | Key::CtrlC      => Some(Action::Cancel),
        // `Key` is the console crate's enum; every other key is ignored.
        _                                              => None,
    }
}

/// Result of one key press.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Keep the list open.
    Stay,
    /// User pressed `d`; holds the checked indices in list order.
    Confirmed(Vec<usize>),
    /// User pressed `q`, Esc or Ctrl-C.
    Cancelled,
}

/// Everything the list remembers between key presses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListState {
    checked:  Vec<bool>,
    /// Entry under the cursor.
    cursor:   usize,
    /// First entry on screen (the list scrolls when it is taller than the terminal).
    top:      usize,
    /// Entry whose full link is shown, if any.
    expanded: Option<usize>,
}

impl ListState {
    fn new(checked: Vec<bool>) -> Self {
        Self { checked, cursor: 0, top: 0, expanded: None }
    }

    /// Applies one action; `page` is how many entries fit on screen.
    fn apply(&mut self, action: Action, page: usize) -> Step {
        let last = self.checked.len().saturating_sub(1);
        match action {
            Action::Up       => self.cursor = self.cursor.saturating_sub(1),
            Action::Down     => self.cursor = (self.cursor + 1).min(last),
            Action::PageUp   => self.cursor = self.cursor.saturating_sub(page.max(1)),
            Action::PageDown => self.cursor = (self.cursor + page.max(1)).min(last),
            Action::Toggle => {
                if let Some(c) = self.checked.get_mut(self.cursor) {
                    *c = !*c;
                }
            }
            Action::ToggleAll => {
                // All checked → uncheck all; otherwise check all.
                let all_checked = self.checked.iter().all(|&c| c);
                self.checked.fill(!all_checked);
            }
            Action::Expand => {
                self.expanded = if self.expanded == Some(self.cursor) { None } else { Some(self.cursor) };
            }
            Action::Confirm => return Step::Confirmed(self.chosen()),
            Action::Cancel  => return Step::Cancelled,
        }
        Step::Stay
    }

    fn chosen(&self) -> Vec<usize> {
        self.checked
            .iter()
            .enumerate()
            .filter_map(|(i, &c)| c.then_some(i))
            .collect()
    }

    /// Scrolls so the cursor is one of the `rows` entries on screen.
    fn scroll_into_view(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
    }
}

// =============================================================================
// Rendering
// =============================================================================

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy)]
struct Screen {
    rows: usize,
    cols: usize,
}

/// Title + footer + one spare line, so drawing never scrolls the terminal.
const CHROME_LINES: usize = 3;
/// Spaces before an expanded link.
const INDENT: usize = 8;
/// Longest an expanded link may get before it is cut with `…`.
const MAX_LINK_LINES: usize = 4;

/// How many entries fit on screen, leaving room for an expanded link.
fn list_rows(state: &ListState, items: &[Item], screen: Screen) -> usize {
    let link_lines = state
        .expanded
        .and_then(|i| items.get(i))
        .map_or(0, |item| wrap(item.link, link_width(screen)).len());
    screen.rows.saturating_sub(CHROME_LINES + link_lines).max(1)
}

/// Usable width; the last column stays empty so terminals never auto-wrap,
/// which would break the line count used to redraw.
fn line_width(screen: Screen) -> usize {
    screen.cols.saturating_sub(1).max(20)
}

fn link_width(screen: Screen) -> usize {
    line_width(screen) - INDENT
}

/// Draws the title, the visible entries (plus an expanded link) and the key hints.
fn render(state: &ListState, title: &str, items: &[Item], screen: Screen, rows: usize) -> Vec<String> {
    let width = line_width(screen);
    // `truncate_str` measures visible width, ignoring colour codes.
    let fit = |s: String| truncate_str(&s, width, "…").into_owned();

    let mut lines = Vec::with_capacity(rows + MAX_LINK_LINES + 2);
    let checked = state.checked.iter().filter(|&&c| c).count();
    lines.push(fit(format!(
        "{} {}",
        style(title).cyan().bold(),
        style(format!("({checked}/{} checked)", items.len())).dim(),
    )));

    for (i, item) in items.iter().enumerate().skip(state.top).take(rows) {
        let here = i == state.cursor;
        let pointer = if here { style("❯").cyan().bold() } else { style(" ") };
        let checkbox = if state.checked[i] { style("[x]").green().bold() } else { style("[ ]").dim() };
        let label = if here { style(item.label.as_ref()).bold() } else { style(item.label.as_ref()) };
        lines.push(fit(format!("{pointer} {checkbox} {label}  {}", style(shorten_url(item.link)).dim())));

        if state.expanded == Some(i) {
            lines.extend(
                wrap(item.link, link_width(screen))
                    .into_iter()
                    .map(|chunk| format!("{:INDENT$}{}", "", style(chunk).cyan())),
            );
        }
    }

    let shown_to = (state.top + rows).min(items.len());
    let position = if items.len() > rows {
        format!("{}–{} of {} · ", state.top + 1, shown_to, items.len())
    } else {
        String::new()
    };
    lines.push(fit(style(format!(
        "  {position}↑↓ move · space check · a all · enter link · d download · q quit"
    )).dim().to_string()));
    lines
}

/// `https://raw.githubusercontent.com/o/r/main/docs/My%20Book.pdf`
/// → `raw.githubusercontent.com/…/My Book.pdf` (scheme, query and middle of
/// the path dropped; the file name is percent-decoded for readability).
fn shorten_url(link: &str) -> String {
    let rest = link.split_once("://").map_or(link, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        []           => host.to_owned(),
        [only]       => format!("{host}/{}", percent_decode_str(only).decode_utf8_lossy()),
        [.., last]   => format!("{host}/…/{}", percent_decode_str(last).decode_utf8_lossy()),
    }
}

/// Splits `link` into lines of at most `width` characters, at most
/// [`MAX_LINK_LINES`] of them (the last one ends in `…` when cut).
fn wrap(link: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = link.chars().collect();
    let mut lines: Vec<String> = chars
        .chunks(width)
        .take(MAX_LINK_LINES)
        .map(|chunk| chunk.iter().collect())
        .collect();
    if chars.len() > width * MAX_LINK_LINES {
        if let Some(last) = lines.last_mut() {
            last.pop();
            last.push('…');
        }
    }
    lines
}

// =============================================================================
// Terminal helpers
// =============================================================================

/// Erases the `n` lines drawn last (no-op for 0, before the first frame).
fn clear(term: &Term, n: usize) -> io::Result<()> {
    if n > 0 {
        term.clear_last_lines(n)?;
    }
    Ok(())
}

/// Hides the cursor while the list is open and shows it again on drop —
/// also when an error returns early.
struct HiddenCursor<'a>(&'a Term);

impl<'a> HiddenCursor<'a> {
    fn new(term: &'a Term) -> io::Result<Self> {
        term.hide_cursor()?;
        Ok(Self(term))
    }
}

impl Drop for HiddenCursor<'_> {
    fn drop(&mut self) {
        let _ = self.0.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use console::measure_text_width;

    fn items(links: &[&'static str]) -> Vec<Item<'static>> {
        links.iter().map(|&l| Item { label: Cow::Borrowed("file.pdf"), link: l }).collect()
    }

    #[test]
    fn toggle_and_confirm() {
        let mut s = ListState::new(vec![true; 3]);
        assert_eq!(s.apply(Action::Down, 10), Step::Stay);
        s.apply(Action::Toggle, 10);
        assert_eq!(s.apply(Action::Confirm, 10), Step::Confirmed(vec![0, 2]));
    }

    #[test]
    fn toggle_all_flips_between_all_and_none() {
        let mut s = ListState::new(vec![false; 3]);
        s.apply(Action::Toggle, 10); // one checked → "a" checks all
        s.apply(Action::ToggleAll, 10);
        assert_eq!(s.chosen(), vec![0, 1, 2]);
        s.apply(Action::ToggleAll, 10);
        assert!(s.chosen().is_empty());
    }

    #[test]
    fn cursor_stays_in_bounds() {
        let mut s = ListState::new(vec![false; 3]);
        s.apply(Action::Up, 10);
        assert_eq!(s.cursor, 0);
        s.apply(Action::PageDown, 10);
        assert_eq!(s.cursor, 2);
        s.apply(Action::Down, 10);
        assert_eq!(s.cursor, 2);
        s.apply(Action::PageUp, 0); // page 0 still moves
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn enter_expands_and_collapses() {
        let mut s = ListState::new(vec![false; 2]);
        s.apply(Action::Expand, 10);
        assert_eq!(s.expanded, Some(0));
        s.apply(Action::Down, 10);
        s.apply(Action::Expand, 10); // moves the expansion
        assert_eq!(s.expanded, Some(1));
        s.apply(Action::Expand, 10);
        assert_eq!(s.expanded, None);
    }

    #[test]
    fn cancel_keys() {
        for key in [Key::Escape, Key::Char('q'), Key::CtrlC] {
            assert_eq!(action_for(&key), Some(Action::Cancel));
        }
        assert_eq!(action_for(&Key::Char('z')), None);
    }

    #[test]
    fn scrolls_to_keep_cursor_visible() {
        let mut s = ListState::new(vec![false; 100]);
        s.cursor = 50;
        s.scroll_into_view(10);
        assert_eq!(s.top, 41);
        s.cursor = 5;
        s.scroll_into_view(10);
        assert_eq!(s.top, 5);
    }

    #[test]
    fn shortens_links() {
        assert_eq!(
            shorten_url("https://raw.githubusercontent.com/o/r/main/docs/My%20Book.pdf?x=1"),
            "raw.githubusercontent.com/…/My Book.pdf",
        );
        assert_eq!(shorten_url("https://archive.org/details"), "archive.org/details");
        assert_eq!(shorten_url("https://example.com/"), "example.com");
        assert_eq!(shorten_url("not a url"), "not a url");
    }

    #[test]
    fn wrap_caps_long_links() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        let long = "x".repeat(100);
        let lines = wrap(&long, 10);
        assert_eq!(lines.len(), MAX_LINK_LINES);
        assert!(lines[MAX_LINK_LINES - 1].ends_with('…'));
    }

    #[test]
    fn render_fits_screen() {
        let long = "https://example.com/a/very/long/path/that/goes/on/and/on/forever/and/ever/book.pdf";
        let list = items(&[long; 50]);
        let screen = Screen { rows: 12, cols: 40 };
        let mut s = ListState::new(vec![true; list.len()]);
        s.expanded = Some(0);
        let rows = list_rows(&s, &list, screen);
        s.scroll_into_view(rows);
        let lines = render(&s, "Pick", &list, screen, rows);

        assert!(lines.len() < screen.rows, "{} lines drawn on a {}-row screen", lines.len(), screen.rows);
        for line in &lines {
            assert!(measure_text_width(line) < screen.cols, "too wide: {line}");
        }
    }

    #[test]
    fn render_tiny_terminal_still_shows_cursor_row() {
        let list = items(&["https://a.org/x.pdf"; 5]);
        let screen = Screen { rows: 1, cols: 5 };
        let s = ListState::new(vec![false; 5]);
        let rows = list_rows(&s, &list, screen);
        assert_eq!(rows, 1);
        assert_eq!(render(&s, "Pick", &list, screen, rows).len(), 3);
    }
}
