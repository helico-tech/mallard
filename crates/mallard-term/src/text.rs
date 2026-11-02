//! Untrusted text on a terminal.
//!
//! Event data, span names and log bodies are written by any app that can
//! reach the server. [`clean`] and [`clean_text`] make such text safe to
//! print: no character in the result can start an escape sequence, move the
//! cursor or reorder what is shown.

use std::borrow::Cow;

use serde::{Serialize, de::DeserializeOwned};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Bidi overrides and marks: they can make text display in another order
/// than it is stored.
pub fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Control characters (C0, DEL, C1: ESC, OSC, CSI, BEL, CR, …) and bidi
/// controls.
fn is_unsafe(c: char) -> bool {
    c.is_control() || is_bidi_control(c)
}

/// One line of untrusted text, safe to print: newlines and carriage
/// returns become spaces, a tab four spaces, and every other control or
/// bidi character a visible escape such as `\u{1b}`.
pub fn clean(s: &str) -> Cow<'_, str> {
    clean_impl(s, false)
}

/// Like [`clean`], but keeps line breaks (`\r\n` and `\n` become `\n`; a
/// lone `\r` is dropped).
pub fn clean_text(s: &str) -> Cow<'_, str> {
    clean_impl(s, true)
}

fn clean_impl(s: &str, keep_newlines: bool) -> Cow<'_, str> {
    if !s.chars().any(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' if keep_newlines => out.push('\n'),
            '\r' if keep_newlines => {}
            '\n' | '\r' => out.push(' '),
            '\t' => out.push_str("    "),
            c if is_unsafe(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

fn clean_line_or_text(s: &str, keep_newlines: bool) -> String {
    if keep_newlines { clean_text(s).into_owned() } else { clean(s).into_owned() }
}

fn clean_value(v: &mut serde_json::Value, keep_newlines: bool) {
    use serde_json::Value;
    match v {
        Value::String(s) => *s = clean_line_or_text(s, keep_newlines),
        Value::Array(a) => a.iter_mut().for_each(|x| clean_value(x, keep_newlines)),
        Value::Object(o) => {
            let old = std::mem::take(o);
            for (k, mut x) in old {
                clean_value(&mut x, keep_newlines);
                o.insert(clean_line_or_text(&k, keep_newlines), x);
            }
        }
        _ => {}
    }
}

/// [`clean_text`] (or [`clean`]) every string and object key inside an API response, so
/// no field of it can carry terminal escape sequences, also a field added later. Used
/// for all non-JSON output and the TUIs; `--json` output is escaped by serde instead.
/// Fails closed: a value that does not survive the JSON round trip is an error, never
/// returned uncleaned.
pub fn sanitize<T: Serialize + DeserializeOwned>(v: T, keep_newlines: bool) -> Result<T, serde_json::Error> {
    let mut value = serde_json::to_value(&v)?;
    clean_value(&mut value, keep_newlines);
    serde_json::from_value(value)
}

/// Escape what serde_json leaves raw in strings (DEL, C1 controls, bidi
/// controls) as `\uXXXX`. serde already escapes C0, so the only raw newlines
/// are the pretty-printer's own; the result is the same JSON value.
pub fn json_safe(json: &str) -> Cow<'_, str> {
    let unsafe_in_json = |c: char| is_unsafe(c) && c != '\n';
    if !json.chars().any(unsafe_in_json) {
        return Cow::Borrowed(json);
    }
    let mut out = String::with_capacity(json.len() + 8);
    for c in json.chars() {
        if unsafe_in_json(c) {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Terminal columns `s` takes (CJK and emoji take two).
pub fn width(s: &str) -> usize {
    s.width()
}

/// The longest prefix of the chars of `it` that fits in `max` columns.
fn take_columns(it: impl Iterator<Item = char>, max: usize) -> Vec<char> {
    let mut used = 0;
    it.take_while(|c| {
        used += c.width().unwrap_or(0);
        used <= max
    })
    .collect()
}

/// Cut `s` to at most `max` columns, ending with "…" when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = take_columns(s.chars(), max - 1).into_iter().collect();
    out.push('…');
    out
}

/// Cut `s` to at most `max` columns keeping the end: "…/src/app.js:10".
pub fn truncate_left(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::from("…");
    out.extend(take_columns(s.chars().rev(), max - 1).into_iter().rev());
    out
}

/// `s` padded with spaces to `w` columns (`{:<w}` counts chars, not columns).
pub fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(s.width())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_cleans_nested_strings_and_keys() {
        let v = serde_json::json!({"t\x1b[2J": ["x\x1b]0;pwn\x07", {"k": "\u{9b}"}], "n": 1, "m": "a\nb"});
        let s = sanitize(v.clone(), false).unwrap();
        let text = s.to_string();
        assert!(!text.contains('\u{1b}') && !text.contains('\u{9b}') && !text.contains('\u{7}'), "{text}");
        assert!(text.contains("\"n\":1"));
        assert_eq!(s["m"], "a b", "one line");
        assert_eq!(sanitize(v, true).unwrap()["m"], "a\nb", "line breaks kept");
    }

    #[test]
    fn sanitize_fails_closed() {
        #[derive(Serialize, serde::Deserialize, Debug)]
        struct Ratio {
            label: String,
            value: f64,
        }
        // NaN serializes to null, which does not deserialize as f64.
        let r = sanitize(Ratio { label: "\x1b[2Jx".into(), value: f64::NAN }, false);
        assert!(r.is_err(), "{r:?}");
    }

    /// Payloads an app could send: OSC 52 (clipboard), a CSI that clears the
    /// screen, an OSC 8 link, a C1 CSI, a carriage return, a tab and a bidi
    /// override.
    const EVIL: &str = "a\u{1b}]52;c;aGk=\u{7}b\u{1b}[2J\u{1b}]8;;http://x\u{1b}\\c\u{9b}31md\re\tf\u{202e}g";

    // From mallard-sentry-cli: nothing unsafe is left, and the result is exact.
    #[test]
    fn clean_strips_terminal_escapes() {
        let evil = "a\x1b]52;c;ZXZpbA==\x07b\x1b[2Jc\u{9b}31md\re\tf\u{202e}g";
        let out = clean(evil);
        assert!(!out.chars().any(|c| c.is_control()), "{out:?}");
        assert_eq!(out, "a\\u{1b}]52;c;ZXZpbA==\\u{7}b\\u{1b}[2Jc\\u{9b}31md e    f\\u{202e}g");
        assert_eq!(clean_text("l1\r\nl2"), "l1\nl2");
        assert_eq!(clean("l1\nl2"), "l1 l2");
    }

    // From mallard-otel-cli: plain text is borrowed, escapes stay visible, and no
    // unsafe character survives in either mode.
    #[test]
    fn clean_escapes_control_characters() {
        assert_eq!(clean("plain text ok"), "plain text ok");
        assert!(matches!(clean("plain"), Cow::Borrowed(_)));
        assert_eq!(clean("tab\there"), "tab    here");
        for c in [clean(EVIL), clean_text(EVIL)] {
            assert!(!c.chars().any(is_unsafe), "{c:?}");
            assert!(c.contains("\\u{1b}]52"), "escapes stay visible: {c:?}");
        }
        assert!(clean_text("a\nb").contains('\n'));
        assert!(!clean("a\nb").contains('\n'), "one line");
    }

    #[test]
    fn json_safe_escapes_c1_and_bidi_but_stays_equal() {
        let j = json_safe("[\"\u{9b}x\u{202e}\"]");
        assert_eq!(j, "[\"\\u009bx\\u202e\"]");
        let pretty = "{\n  \"t\": \"a\u{9b}31m\u{202e}b\\u001bc\"\n}";
        let out = json_safe(pretty);
        assert!(!out.chars().any(|c| is_unsafe(c) && c != '\n'), "{out:?}");
        assert!(out.contains("\\u009b") && out.contains("\\u202e") && out.contains('\n'));
        assert!(matches!(json_safe("{}"), Cow::Borrowed(_)));
    }

    #[test]
    fn cutting_counts_columns() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("x", 0), "");
        assert_eq!(truncate_left("/a/b/c/app.js:10", 10), "…app.js:10");
        assert_eq!(truncate_left("app.js", 10), "app.js");
        // Wide characters count as two columns.
        assert_eq!(truncate("日本語のテキスト", 7), "日本語…");
        assert_eq!(width(&truncate("日本語のテキスト", 7)), 7);
        assert_eq!(truncate_left("/パス/ファイル.js", 10), "…ァイル.js");
        assert_eq!(pad("日本", 6), "日本  ");
        assert_eq!(pad("toolong", 3), "toolong");
    }
}
