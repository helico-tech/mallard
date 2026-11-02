//! The suggestion list of a filter bar, the same on every tab. No I/O in here:
//! edits go in, [`Suggest::poll`] says what to fetch (debounced), the tab fetches it in
//! the background and hands the answer to [`Suggest::answer`]. So typing can never wait
//! on the network, and a slow or failed fetch costs at most a small note.
//!
//! Keys first, then the values of a key. What is known shows at once from the cache; the
//! cache holds per scope (window and app), and answers for another scope are dropped.
//! Fetched text is untrusted, and it is also a name the server only knows as it is. So the
//! list keeps it raw: a row shows it cleaned ([`Item::label`]), and Tab puts it in the bar raw
//! ([`Item::insert`]), because a cleaned name filters on nothing. A bar therefore holds raw
//! text and must clean it where it draws it (ADR 0006).

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use super::{rounded, theme};
use crate::fmt::compact;
use crate::text::{clean, truncate, width};

/// Ask for this many keys or values. A full answer may hide some, so a typed prefix then
/// asks the server too.
pub const LIMIT: u32 = 100;
/// How long a fetch may take before it counts as failed. For the tabs to apply.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const DEBOUNCE: Duration = Duration::from_millis(200);
/// How long a cached answer holds, and a failed fetch waits before it is tried again.
const FRESH: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(10);
const MAX_ROWS: usize = 8;
const MAX_WIDTH: u16 = 64;
const NOTE: &str = "suggestions unavailable";

/// What to fetch: the keys (`key` is `None`), or the values of a key. `prefix` is empty
/// unless the answer without one was full.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Want {
    pub key: Option<String>,
    pub prefix: String,
}

/// Where the values of a built-in key come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Values {
    /// Nothing to suggest (`min=`, `trace=`).
    None,
    Fixed(&'static [&'static str]),
    Fetch,
}

/// A key of the filter language, as it is typed: `service=`, or `errors` without a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub text: &'static str,
    pub values: Values,
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What it shows: cleaned, safe for a terminal.
    pub label: String,
    /// What Tab puts in place of the token, with the server's text as it is. Never draw it
    /// without cleaning it.
    pub insert: String,
    pub count: Option<u64>,
}

struct Entry {
    /// As the server has them.
    items: Vec<(String, u64)>,
    at: Instant,
}

/// The token under the cursor.
#[derive(Debug, PartialEq, Eq)]
struct Token {
    /// Byte range of the whole token, also what is after the cursor.
    range: Range<usize>,
    /// `Some` when the cursor is after `key=`.
    key: Option<String>,
    prefix: String,
    /// Inside a quoted word: free text, nothing to suggest.
    free: bool,
}

/// The list of one filter bar.
pub struct Suggest {
    keys: &'static [Key],
    scope: String,
    cache: HashMap<Want, Entry>,
    failed: HashMap<Want, Instant>,
    text: String,
    /// In chars, like the inputs.
    cursor: usize,
    open: bool,
    /// When the text changed, until the debounce is over.
    edited: Option<Instant>,
    in_flight: Option<Want>,
    items: Vec<Item>,
    selected: usize,
    note: bool,
}

impl Suggest {
    pub fn new(keys: &'static [Key]) -> Self {
        Suggest {
            keys,
            scope: String::new(),
            cache: HashMap::new(),
            failed: HashMap::new(),
            text: String::new(),
            cursor: 0,
            open: false,
            edited: None,
            in_flight: None,
            items: vec![],
            selected: 0,
            note: false,
        }
    }

    /// The window and the app. Another one forgets the cache, and answers still on their
    /// way for the old one are ignored.
    pub fn set_scope(&mut self, scope: String) {
        if scope != self.scope {
            self.scope = scope;
            self.cache.clear();
            self.failed.clear();
            self.in_flight = None;
            self.note = false;
            if self.open {
                self.edited.get_or_insert_with(Instant::now);
            }
            self.refill();
        }
    }

    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// The bar opened. A fetch of an earlier time that never answered is forgotten.
    pub fn open(&mut self, text: &str, cursor: usize, now: Instant) {
        self.open = true;
        self.in_flight = None;
        self.edited(text, cursor, now);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.edited = None;
        self.in_flight = None;
        self.items.clear();
        self.note = false;
    }

    /// The text or the cursor changed. Never waits: the list comes from the cache.
    pub fn edited(&mut self, text: &str, cursor: usize, now: Instant) {
        self.text = text.to_string();
        self.cursor = cursor;
        self.selected = 0;
        self.edited = Some(now);
        self.refill();
    }

    /// What to fetch now, if anything: once the typing has paused. The tab should drop a
    /// fetch it still has running.
    pub fn poll(&mut self, now: Instant) -> Option<Want> {
        if !self.open || self.edited.is_none_or(|t| now.duration_since(t) < DEBOUNCE) {
            return None;
        }
        self.edited = None;
        let want = self.needed(now)?;
        if self.in_flight.as_ref() == Some(&want) {
            return None;
        }
        self.in_flight = Some(want.clone());
        Some(want)
    }

    /// The tab's answer: `(text, count)`, or `None` when the fetch failed. An answer for
    /// another scope is ignored; one for an earlier token only fills the cache.
    pub fn answer(&mut self, scope: &str, want: &Want, answer: Option<Vec<(String, u64)>>, now: Instant) {
        if scope != self.scope {
            return;
        }
        if self.in_flight.as_ref() == Some(want) {
            self.in_flight = None;
        }
        match answer {
            Some(items) => {
                self.cache.insert(want.clone(), Entry { items, at: now });
                self.failed.remove(want);
                self.note = false;
                // A full answer may mean the typed prefix needs its own fetch.
                if self.open {
                    self.edited = Some(now.checked_sub(DEBOUNCE).unwrap_or(now));
                }
            }
            None => {
                self.failed.insert(want.clone(), now);
                self.note = self.open;
            }
        }
        self.refill();
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn note(&self) -> Option<&'static str> {
        self.note.then_some(NOTE)
    }

    /// Up (-1) or down (1) in the list, wrapping around.
    pub fn select(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        }
    }

    /// Tab: the text with the selected row in place of the token under the cursor, and
    /// the cursor (in chars) after it. `None` with nothing to accept.
    pub fn accept(&mut self, now: Instant) -> Option<(String, usize)> {
        let item = self.items.get(self.selected)?;
        let t = token(&self.text, self.cursor);
        let mut text = self.text.clone();
        text.replace_range(t.range.clone(), &item.insert);
        let cursor = text[..t.range.start].chars().count() + item.insert.chars().count();
        self.edited(&text, cursor, now);
        Some((text, cursor))
    }

    /// The list above the bar `footer`, inside `area`.
    pub fn render(&self, f: &mut Frame, area: Rect, footer: Rect) {
        let rows = self.items.len().min(MAX_ROWS);
        let note = self.note();
        let height = (rows + usize::from(note.is_some())) as u16 + 2;
        let room = footer.y.saturating_sub(area.y);
        if (rows == 0 && note.is_none()) || height > room || footer.width < 16 {
            return;
        }
        let w = MAX_WIDTH.min(footer.width);
        let r = Rect { x: footer.x, y: footer.y - height, width: w, height };
        f.render_widget(Clear, r);
        let mut block = rounded();
        if rows > 0 {
            block = block
                .title_bottom(Line::from(Span::styled(" tab accepts · ↑↓ choose ", theme::faint())).right_aligned());
        }
        let inner = block.inner(r);
        f.render_widget(block, r);
        // The window of rows that shows the selected one.
        let first = self.selected.saturating_sub(rows.saturating_sub(1));
        let cols = inner.width as usize;
        let mut lines = Vec::with_capacity(rows + 1);
        for (i, item) in self.items.iter().enumerate().skip(first).take(rows) {
            let count = item.count.map_or(String::new(), |n| compact(n as f64));
            let room = cols.saturating_sub(width(&count) + 2);
            let label = truncate(&item.label, room);
            let gap = cols.saturating_sub(width(&label) + width(&count) + 1);
            let style = if i == self.selected { theme::selected() } else { theme::text() };
            lines.push(Line::from(vec![
                Span::styled(format!(" {label}{}", " ".repeat(gap)), style),
                Span::styled(count, if i == self.selected { theme::selected() } else { theme::faint() }),
            ]));
        }
        if let Some(n) = note {
            lines.push(Line::from(Span::styled(format!(" {n}"), theme::faint())));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn fresh(&self, want: &Want, now: Instant) -> Option<&Entry> {
        self.cache.get(want).filter(|e| now.duration_since(e.at) < FRESH)
    }

    /// The fetch the current token still needs: first without a prefix; with one only
    /// when that answer was full.
    fn needed(&self, now: Instant) -> Option<Want> {
        let t = token(&self.text, self.cursor);
        if t.free {
            return None;
        }
        if let Some(k) = &t.key
            && self.builtin_values(k).is_some_and(|v| v != Values::Fetch)
        {
            return None;
        }
        let base = Want { key: t.key.clone(), prefix: String::new() };
        let wanted = match self.fresh(&base, now) {
            None => base,
            Some(e) if e.items.len() as u32 >= LIMIT && !t.prefix.is_empty() => Want { key: t.key, prefix: t.prefix },
            Some(_) => return None,
        };
        let tried = self.fresh(&wanted, now).is_some()
            || self.failed.get(&wanted).is_some_and(|at| now.duration_since(*at) < RETRY);
        (!tried).then_some(wanted)
    }

    fn builtin_values(&self, key: &str) -> Option<Values> {
        self.keys.iter().find(|k| k.text.strip_suffix('=') == Some(key)).map(|k| k.values)
    }

    /// The rows for the current text, from the built-in keys and the cache.
    fn refill(&mut self) {
        let keep = self.items.get(self.selected).map(|i| i.insert.clone());
        self.items = if self.open { self.rows() } else { vec![] };
        self.selected = keep.and_then(|k| self.items.iter().position(|i| i.insert == k)).unwrap_or(0);
    }

    fn rows(&self) -> Vec<Item> {
        let t = token(&self.text, self.cursor);
        if t.free {
            return vec![];
        }
        // Whatever the cache has for this token, with or without a prefix.
        let cached = |key: &Option<String>| -> Vec<(String, u64)> {
            let mut out: Vec<(String, u64)> = vec![];
            let wants =
                [Want { key: key.clone(), prefix: String::new() }, Want { key: key.clone(), prefix: t.prefix.clone() }];
            for w in &wants {
                for (text, n) in self.cache.get(w).map_or(&[][..], |e| &e.items) {
                    // The bar holds raw text, but what one can read and type is the label.
                    let fits = starts_with(text, &t.prefix) || starts_with(&clean(text), &t.prefix);
                    if fits && !out.iter().any(|(o, _)| o == text) {
                        out.push((text.clone(), *n));
                    }
                }
            }
            out
        };
        match &t.key {
            None => {
                let mut rows: Vec<Item> = self
                    .keys
                    .iter()
                    .filter(|k| starts_with(k.text, &t.prefix))
                    .map(|k| Item { label: k.text.to_string(), insert: key_insert(k.text), count: None })
                    .collect();
                for (text, n) in cached(&None) {
                    // An attribute named like a built-in key could not be filtered on.
                    let builtin = self.keys.iter().any(|k| k.text.trim_end_matches('=') == text);
                    if !builtin && !text.contains(|c: char| c.is_whitespace() || c == '"' || c == '=') {
                        rows.push(Item {
                            label: format!("{}=", clean(&text)),
                            insert: format!("{text}="),
                            count: Some(n),
                        });
                    }
                }
                rows
            }
            Some(key) => match self.builtin_values(key) {
                Some(Values::None) => vec![],
                Some(Values::Fixed(values)) => values
                    .iter()
                    .filter(|v| starts_with(v, &t.prefix))
                    .map(|v| Item { label: v.to_string(), insert: value_insert(key, v), count: None })
                    .collect(),
                _ => cached(&t.key)
                    .into_iter()
                    .filter(|(text, _)| !text.contains('"'))
                    .map(|(text, n)| Item {
                        label: clean(&text).into_owned(),
                        insert: value_insert(key, &text),
                        count: Some(n),
                    })
                    .collect(),
            },
        }
    }
}

fn starts_with(text: &str, prefix: &str) -> bool {
    text.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// A key with a value goes on with its value; a bare one ends the token.
fn key_insert(text: &str) -> String {
    if text.ends_with('=') { text.to_string() } else { format!("{text} ") }
}

fn value_insert(key: &str, value: &str) -> String {
    if value.contains(char::is_whitespace) { format!("{key}=\"{value}\" ") } else { format!("{key}={value} ") }
}

fn token(text: &str, cursor: usize) -> Token {
    let cut = text.char_indices().nth(cursor).map_or(text.len(), |(i, _)| i);
    let range = word_ranges(text).into_iter().find(|r| r.start <= cut && cut <= r.end).unwrap_or(cut..cut);
    let before = &text[range.start..cut];
    match before.split_once('=') {
        Some((key, value)) if !key.is_empty() && !key.contains('"') => {
            Token { range, key: Some(key.to_string()), prefix: value.trim_start_matches('"').to_string(), free: false }
        }
        _ => Token { free: before.contains('"'), range, key: None, prefix: before.to_string() },
    }
}

/// Byte ranges of the words of `s`: split on whitespace, except inside `"quotes"` (also
/// in `key="v w"`).
pub fn word_ranges(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && !quoted {
            if let Some(b) = start.take() {
                out.push(b..i);
            }
        } else {
            start.get_or_insert(i);
            if c == '"' {
                quoted = !quoted;
            }
        }
    }
    if let Some(b) = start {
        out.push(b..s.len());
    }
    out
}

/// The words of `s` without their quotes. An empty `""` stays an empty word.
pub fn tokenize(s: &str) -> Vec<String> {
    word_ranges(s).into_iter().map(|r| s[r].replace('"', "")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    const KEYS: &[Key] = &[
        Key { text: "service=", values: Values::Fetch },
        Key { text: "level=", values: Values::Fixed(&["info", "warn", "error"]) },
        Key { text: "min=", values: Values::None },
        Key { text: "errors", values: Values::None },
    ];

    fn want(key: Option<&str>, prefix: &str) -> Want {
        Want { key: key.map(str::to_string), prefix: prefix.to_string() }
    }

    fn open(text: &str) -> (Suggest, Instant) {
        let now = Instant::now();
        let mut s = Suggest::new(KEYS);
        s.set_scope("1h".into());
        s.open(text, text.chars().count(), now);
        (s, now)
    }

    fn labels(s: &Suggest) -> Vec<&str> {
        s.items().iter().map(|i| i.label.as_str()).collect()
    }

    const LATER: Duration = Duration::from_millis(250);

    #[test]
    fn keys_first_at_once_then_fetched_keys() {
        let (mut s, now) = open("");
        assert_eq!(labels(&s), ["service=", "level=", "min=", "errors"], "built-in keys do not wait");
        assert_eq!(s.poll(now), None, "debounced");
        assert_eq!(s.poll(now + LATER), Some(want(None, "")));
        assert_eq!(s.poll(now + LATER), None, "asked once");
        let answer = vec![("http.route".to_string(), 5), ("service".to_string(), 9), ("has space".to_string(), 1)];
        s.answer("1h", &want(None, ""), Some(answer), now + LATER);
        assert_eq!(labels(&s), ["service=", "level=", "min=", "errors", "http.route="]);
        assert_eq!(s.items()[4].count, Some(5));
    }

    #[test]
    fn values_of_a_key_and_tab() {
        let (mut s, now) = open("errors serv");
        assert_eq!(labels(&s), ["service="]);
        let (text, cursor) = s.accept(now).unwrap();
        assert_eq!((text.as_str(), cursor), ("errors service=", 15));
        assert!(!s.has_items(), "values are not known yet");
        assert_eq!(s.poll(now + LATER), Some(want(Some("service"), "")));
        let values = vec![("checkout api".to_string(), 7), ("web".to_string(), 2)];
        s.answer("1h", &want(Some("service"), ""), Some(values), now + LATER);
        assert_eq!(labels(&s), ["checkout api", "web"]);
        s.select(1);
        s.select(1);
        assert_eq!(s.selected(), 0, "wraps around");
        let (text, _) = s.accept(now).unwrap();
        assert_eq!(text, "errors service=\"checkout api\" ", "quoted, and the next token starts");
    }

    #[test]
    fn tab_replaces_the_token_under_the_cursor() {
        let now = Instant::now();
        let mut s = Suggest::new(KEYS);
        s.set_scope("1h".into());
        s.open("lev x", 3, now);
        assert_eq!(labels(&s), ["level="]);
        let (text, cursor) = s.accept(now).unwrap();
        assert_eq!((text.as_str(), cursor), ("level= x", 6));
        assert_eq!(labels(&s), ["info", "warn", "error"], "fixed values need no fetch");
        assert_eq!(s.poll(now + LATER), None);
        s.edited("level=w x", 7, now);
        assert_eq!(labels(&s), ["warn"]);
        s.edited("min=", 4, now);
        assert!(!s.has_items());
        assert_eq!(s.poll(now + LATER), None, "nothing to fetch for min=");
        s.edited("\"free te", 8, now);
        assert!(!s.has_items());
        assert_eq!(s.poll(now + 2 * LATER), None, "free text");
        s.edited("\"a=b", 4, now);
        assert!(!s.has_items());
        assert_eq!(s.poll(now + 3 * LATER), None, "`=` in quotes starts no value");
    }

    #[test]
    fn stale_answers_and_scopes() {
        let (mut s, now) = open("service=");
        s.set_scope("24h".into());
        s.answer("1h", &want(Some("service"), ""), Some(vec![("old".into(), 1)]), now);
        assert!(!s.has_items(), "an answer for another scope is ignored");
        // An answer for an earlier token fills the cache but does not show.
        s.answer("24h", &want(Some("other"), ""), Some(vec![("x".into(), 1)]), now);
        assert!(!s.has_items());
        s.answer("24h", &want(Some("service"), ""), Some(vec![("new".into(), 1)]), now);
        assert_eq!(labels(&s), ["new"]);
        s.set_scope("7d".into());
        assert!(!s.has_items(), "another window forgets the cache");
    }

    #[test]
    fn a_failed_fetch_keeps_the_text_and_is_only_a_note() {
        let (mut s, now) = open("service=we");
        let w = s.poll(now + LATER).unwrap();
        s.answer("1h", &w, None, now + LATER);
        assert_eq!(s.note(), Some(NOTE));
        assert_eq!(s.text, "service=we");
        s.edited("service=web", 11, now + LATER);
        assert_eq!(s.poll(now + 2 * LATER), None, "not again right away");
        s.edited("service=web", 11, now + RETRY + LATER);
        assert_eq!(s.poll(now + RETRY + 2 * LATER), Some(w.clone()), "tried again later");
        s.answer("1h", &w, Some(vec![("web".into(), 1)]), now + RETRY + 2 * LATER);
        assert_eq!(s.note(), None);
    }

    #[test]
    fn a_full_answer_asks_again_with_the_prefix() {
        let (mut s, now) = open("service=zz");
        let w = s.poll(now + LATER).unwrap();
        let full: Vec<(String, u64)> = (0..LIMIT).map(|i| (format!("v{i}"), 1)).collect();
        s.answer("1h", &w, Some(full), now + LATER);
        assert!(!s.has_items());
        assert_eq!(s.poll(now + LATER), Some(want(Some("service"), "zz")));
        s.answer("1h", &want(Some("service"), "zz"), Some(vec![("zzz".into(), 1)]), now + LATER);
        assert_eq!(labels(&s), ["zzz"]);
        assert_eq!(s.poll(now + 2 * LATER), None);
    }

    #[test]
    fn a_fetch_that_never_answers_is_forgotten_when_the_bar_opens_again() {
        let (mut s, now) = open("");
        assert!(s.poll(now + LATER).is_some());
        s.close();
        s.open("", 0, now + LATER);
        assert!(s.poll(now + 2 * LATER).is_some());
    }

    #[test]
    fn old_answers_are_asked_for_again() {
        let (mut s, now) = open("service=");
        let w = s.poll(now + LATER).unwrap();
        s.answer("1h", &w, Some(vec![("a".into(), 1)]), now);
        s.edited("service=", 8, now + FRESH);
        assert_eq!(labels(&s), ["a"], "shown while it is asked for again");
        assert_eq!(s.poll(now + FRESH + LATER), Some(w));
    }

    /// Server text never reaches the terminal raw, and goes back as the server has it: a
    /// row shows it cleaned and Tab inserts it raw. The bars draw that cleaned; each tab
    /// tests its own, through to the request it sends.
    #[test]
    fn a_row_shows_cleaned_and_inserts_raw() {
        let (mut s, now) = open("service=");
        let evil = "a\x1b[2J\u{202e}b";
        s.answer("1h", &want(Some("service"), ""), Some(vec![(evil.to_string(), 3)]), now);
        let item = &s.items()[0];
        assert!(!item.label.chars().any(|c| c.is_control() || crate::text::is_bidi_control(c)), "{item:?}");
        assert_eq!(item.label, "a\\u{1b}[2J\\u{202e}b");
        assert_eq!(item.insert, format!("service={evil} "));
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| s.render(f, f.area(), Rect::new(0, 11, 40, 1))).unwrap();
        let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(!screen.chars().any(|c| c.is_control() || crate::text::is_bidi_control(c)), "{screen:?}");
        assert!(screen.contains("a\\u{1b}[2J"), "{screen}");
        let (text, cursor) = s.accept(now).unwrap();
        assert_eq!(text, format!("service={evil} "), "the value that gets sent");
        assert_eq!(cursor, text.chars().count());

        // A key too: a tag or attribute with such a name is only found by its real name.
        let (mut s, now) = open("");
        s.answer("1h", &want(None, ""), Some(vec![("k\u{202e}".to_string(), 3)]), now);
        let item = s.items().iter().find(|i| i.count.is_some()).unwrap().clone();
        assert_eq!((item.label.as_str(), item.insert.as_str()), ("k\\u{202e}=", "k\u{202e}="));
        s.select(s.items().iter().position(|i| *i == item).unwrap() as isize);
        let (text, _) = s.accept(now).unwrap();
        assert_eq!(text, "k\u{202e}=");
        assert_eq!(s.poll(now + LATER), Some(want(Some("k\u{202e}"), "")), "its values are asked for by that name");
    }

    /// What is typed matches the name as shown and as it is, also when it is edited back.
    #[test]
    fn a_prefix_matches_the_raw_name_and_the_label() {
        let (mut s, now) = open("service=a\\u{20");
        s.answer("1h", &want(Some("service"), ""), Some(vec![("a\u{202e}b".to_string(), 1)]), now);
        assert_eq!(labels(&s), ["a\\u{202e}b"], "typed as it shows");
        s.edited("service=a\u{202e}", 10, now);
        assert_eq!(labels(&s), ["a\\u{202e}b"], "the raw text from an earlier Tab");
        s.edited("service=b", 9, now);
        assert!(!s.has_items());
    }

    /// A name with a line break takes its quotes from the raw text, or it would be two words.
    #[test]
    fn a_raw_name_with_a_line_break_stays_one_word() {
        let (mut s, now) = open("service=");
        s.answer("1h", &want(Some("service"), ""), Some(vec![("a\nb".to_string(), 1)]), now);
        assert_eq!(labels(&s), ["a b"]);
        let (text, _) = s.accept(now).unwrap();
        assert_eq!(tokenize(&text), ["service=a\nb"]);
    }

    #[test]
    fn it_draws_above_the_bar_and_only_with_room() {
        let (mut s, now) = open("");
        s.answer("1h", &want(None, ""), Some(vec![("http.route".to_string(), 1200)]), now);
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| s.render(f, f.area(), Rect::new(0, 11, 40, 1))).unwrap();
        let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("http.route="), "{screen}");
        assert!(screen.contains("1.2k"), "{screen}");
        assert!(screen.contains("tab accepts"), "{screen}");
        let mut t = Terminal::new(TestBackend::new(40, 3)).unwrap();
        t.draw(|f| s.render(f, f.area(), Rect::new(0, 2, 40, 1))).unwrap();
        let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(!screen.contains("service"), "no room: {screen}");
        // A failed fetch with nothing else to show is one small note.
        let (mut s, now) = open("service=");
        let w = s.poll(now + LATER).unwrap();
        s.answer("1h", &w, None, now);
        let mut t = Terminal::new(TestBackend::new(40, 12)).unwrap();
        t.draw(|f| s.render(f, f.area(), Rect::new(0, 11, 40, 1))).unwrap();
        let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains(NOTE), "{screen}");
    }

    #[test]
    fn words_keep_their_quotes_apart() {
        assert_eq!(tokenize(r#"op="GET /cart" errors"#), vec!["op=GET /cart", "errors"]);
        assert_eq!(tokenize("  a   b "), vec!["a", "b"]);
        assert_eq!(tokenize(r#""" x"#), vec!["", "x"]);
        assert_eq!(word_ranges(r#"a="b c" d"#), vec![0..7, 8..9]);
    }
}
