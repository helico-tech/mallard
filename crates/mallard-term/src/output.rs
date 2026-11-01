//! The output contract both CLIs follow (ADR 0008):
//!
//! - `--format text|json|ndjson` ([`Format`]); `--json` is `--format json`.
//! - JSON is pretty on a terminal and compact when piped.
//! - A list is one envelope ([`List`]): `items`, `count`, `limit`,
//!   `truncated`, `next_cursor`, `window`. With `ndjson` each item is one
//!   line and the envelope is left out.
//! - `--fields a,b` keeps only those top-level keys of each item (or of the
//!   object); an unknown field is a usage error that lists the valid ones.
//! - stdout has data only; notes and errors go to stderr. `--quiet` drops
//!   the notes ([`Output::note`]); warnings and errors still print.

use std::io::IsTerminal;

use serde::Serialize;
use serde_json::Value;

use crate::color::{ColorChoice, Out};
use crate::error::usage;
use crate::text::{clean, json_safe};
use crate::window::Window;

/// `--format`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Tables and text for people (plain, without boxes or colour, when piped).
    #[default]
    Text,
    /// One JSON document (compact when piped).
    Json,
    /// One JSON object per line: list items, stream records.
    Ndjson,
}

impl Format {
    /// `--json` wins over `--format` (or `MALLARD_FORMAT`); default text.
    pub fn pick(format: Option<Format>, json: bool) -> Format {
        if json { Format::Json } else { format.unwrap_or_default() }
    }

    pub fn is_json(self) -> bool {
        self != Format::Text
    }
}

/// A list of items and what is known about the rest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct List<T> {
    pub items: Vec<T>,
    pub count: usize,
    /// The `--limit` asked for, when the command has one.
    pub limit: Option<u32>,
    /// More items exist (or may exist) than shown.
    pub truncated: bool,
    /// Pass to the server to continue (where supported).
    pub next_cursor: Option<String>,
    /// The time window searched, when the command has one.
    pub window: Option<Window>,
}

impl<T> List<T> {
    pub fn new(items: Vec<T>) -> Self {
        List { count: items.len(), items, limit: None, truncated: false, next_cursor: None, window: None }
    }

    /// Records the limit; a full page counts as truncated.
    pub fn limit(mut self, limit: Option<u32>) -> Self {
        self.limit = limit;
        if limit.is_some_and(|l| self.count >= l as usize) {
            self.truncated = true;
        }
        self
    }

    pub fn truncated(mut self, truncated: bool) -> Self {
        self.truncated |= truncated;
        self
    }

    /// A cursor means there is more.
    pub fn cursor(mut self, cursor: Option<String>) -> Self {
        self.truncated |= cursor.is_some();
        self.next_cursor = cursor;
        self
    }

    pub fn window(mut self, window: Window) -> Self {
        self.window = Some(window);
        self
    }
}

/// How a command prints: the format, colour, terminal and the `--fields`
/// and `--raw` flags. Build it once from the global flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub format: Format,
    pub out: Out,
    /// stdout is a terminal: pretty JSON, boxed tables, sparklines.
    pub tty: bool,
    pub fields: Option<Vec<String>>,
    /// Include raw payloads (Sentry `payload`, resource attributes).
    pub raw: bool,
    /// `--quiet`: no notes on stderr (truncation, progress, fallbacks).
    pub quiet: bool,
}

impl Default for Output {
    fn default() -> Self {
        Output::new(Format::Text, ColorChoice::Auto)
    }
}

impl Output {
    pub fn new(format: Format, color: ColorChoice) -> Self {
        Output {
            format,
            out: Out::stdout(color),
            tty: std::io::stdout().is_terminal(),
            fields: None,
            raw: false,
            quiet: false,
        }
    }

    /// `--fields a,b,c` (empty names dropped; empty list = all fields).
    pub fn with_fields(mut self, fields: Option<&str>) -> Self {
        self.fields = fields
            .map(|f| f.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>())
            .filter(|f| !f.is_empty());
        self
    }

    pub fn with_raw(mut self, raw: bool) -> Self {
        self.raw = raw;
        self
    }

    pub fn with_quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// `note: …` on stderr, unless `--quiet`. For what helps a person but
    /// is not data, a warning or an error.
    pub fn note(&self, message: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("note: {}", clean(message.as_ref()));
        }
    }

    /// The command prints its own text (tables, details): text format and
    /// no `--fields`.
    pub fn text(&self) -> bool {
        self.format == Format::Text && self.fields.is_none()
    }

    pub fn json(&self) -> bool {
        self.format.is_json()
    }

    /// Print one object (`show`, `status`, `setup`).
    pub fn object<T: Serialize + ?Sized>(&self, v: &T) -> anyhow::Result<()> {
        match &self.fields {
            Some(f) => {
                let v = serde_json::to_value(v)?;
                let sel = select(v.clone(), f, &valid_keys(std::slice::from_ref(&v)))?;
                if self.format == Format::Text {
                    let w = sel.0.iter().map(|(k, _)| crate::text::width(k)).max().unwrap_or(0);
                    for (k, v) in &sel.0 {
                        self.out.line(&format!("{}  {}", crate::text::pad(&clean(k), w), text_value(v)))?;
                    }
                    return Ok(());
                }
                self.emit_object(&sel)
            }
            None => self.emit_object(v),
        }
    }

    fn emit_object<T: Serialize + ?Sized>(&self, v: &T) -> anyhow::Result<()> {
        match self.format {
            Format::Text => self.text_object(&serde_json::to_value(v)?),
            Format::Json => self.json_doc(v),
            Format::Ndjson => self.json_line(v),
        }
    }

    /// Print a list: the envelope (json), one item per line (ndjson, with a
    /// note on stderr when truncated) or, with `--fields`, one line per item.
    /// Without `--fields`, text lists are printed by the command itself.
    pub fn list<T: Serialize>(&self, l: &List<T>) -> anyhow::Result<()> {
        match &self.fields {
            Some(f) => {
                let values = l.items.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>()?;
                let valid = valid_keys(&values);
                let items = values.into_iter().map(|v| select(v, f, &valid)).collect::<anyhow::Result<Vec<_>>>()?;
                if self.format == Format::Text {
                    for sel in &items {
                        self.out.line(&sel.text_line())?;
                    }
                    self.truncation_note(l);
                    return Ok(());
                }
                let sel = List {
                    items,
                    count: l.count,
                    limit: l.limit,
                    truncated: l.truncated,
                    next_cursor: l.next_cursor.clone(),
                    window: l.window,
                };
                self.emit_list(&sel)
            }
            None => self.emit_list(l),
        }
    }

    fn emit_list<T: Serialize>(&self, l: &List<T>) -> anyhow::Result<()> {
        match self.format {
            Format::Json => self.json_doc(l)?,
            Format::Ndjson => {
                for v in &l.items {
                    self.json_line(v)?;
                }
            }
            Format::Text => {
                for v in &l.items {
                    self.out.line(&Selected::of(serde_json::to_value(v)?).text_line())?;
                }
            }
        }
        self.truncation_note(l);
        Ok(())
    }

    /// One record of a stream (`logs -f`): a JSON line in json/ndjson, else
    /// the selected fields on one line.
    pub fn record<T: Serialize>(&self, v: &T) -> anyhow::Result<()> {
        if let Some(f) = &self.fields {
            let val = serde_json::to_value(v)?;
            let sel = select(val.clone(), f, &valid_keys(std::slice::from_ref(&val)))?;
            return if self.json() { self.json_line(&sel) } else { Ok(self.out.line(&sel.text_line())?) };
        }
        if self.json() {
            self.json_line(v)
        } else {
            Ok(self.out.line(&Selected::of(serde_json::to_value(v)?).text_line())?)
        }
    }

    /// "N shown; more exist" on stderr (text and ndjson only: JSON has
    /// `truncated` in the envelope, and stderr may be merged into stdout).
    pub fn truncation_note<T>(&self, l: &List<T>) {
        if l.truncated && self.format != Format::Json {
            self.note(format!("{} shown, more exist; narrow the window or filters, or raise --limit", l.count));
        }
    }

    fn json_doc<T: Serialize + ?Sized>(&self, v: &T) -> anyhow::Result<()> {
        let s = if self.tty { serde_json::to_string_pretty(v)? } else { serde_json::to_string(v)? };
        self.out.line(&json_safe(&s))?;
        Ok(())
    }

    fn json_line<T: Serialize + ?Sized>(&self, v: &T) -> anyhow::Result<()> {
        self.out.line(&json_safe(&serde_json::to_string(v)?))?;
        Ok(())
    }

    /// `key  value` lines.
    fn text_object(&self, v: &Value) -> anyhow::Result<()> {
        match v {
            Value::Object(m) => {
                let w = m.keys().map(|k| crate::text::width(k)).max().unwrap_or(0);
                for (k, v) in m {
                    self.out.line(&format!("{}  {}", crate::text::pad(&clean(k), w), text_value(v)))?;
                }
                Ok(())
            }
            v => Ok(self.out.line(&text_value(v))?),
        }
    }
}

/// A value on one text line: strings as they are (cleaned), null as `-`,
/// anything else as compact JSON.
fn text_value(v: &Value) -> String {
    match v {
        Value::String(s) => clean(s).into_owned(),
        Value::Null => "-".into(),
        v => json_safe(&v.to_string()).into_owned(),
    }
}

/// Top-level keys of the objects, in first-seen order.
fn valid_keys(items: &[Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for v in items {
        if let Value::Object(m) = v {
            for k in m.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    keys
}

/// Some fields of an object, in the order asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected(Vec<(String, Value)>);

impl Selected {
    /// All fields of an object (any value: one field).
    fn of(v: Value) -> Selected {
        match v {
            Value::Object(m) => Selected(m.into_iter().collect()),
            v => Selected(vec![("value".into(), v)]),
        }
    }

    /// The values on one line, two spaces apart.
    fn text_line(&self) -> String {
        self.0.iter().map(|(_, v)| text_value(v)).collect::<Vec<_>>().join("  ")
    }
}

impl Serialize for Selected {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// Keep `fields` of an object, in that order (missing ones as null). A
/// field no item has is a usage error (unless there are no items to know
/// the fields from). Not an object: kept whole under its only field.
fn select(v: Value, fields: &[String], valid: &[String]) -> anyhow::Result<Selected> {
    if !valid.is_empty()
        && let Some(bad) = fields.iter().find(|f| !valid.contains(f))
    {
        return Err(usage(format!("unknown field {bad:?}; fields: {}", valid.join(","))));
    }
    let mut m = match v {
        Value::Object(m) => m,
        v => return Ok(Selected::of(v)),
    };
    Ok(Selected(fields.iter().map(|f| (f.clone(), m.remove(f).unwrap_or(Value::Null))).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn out(format: Format, tty: bool) -> Output {
        Output { format, out: Out { color: false }, tty, fields: None, raw: false, quiet: false }
    }

    #[test]
    fn format_pick() {
        assert_eq!(Format::pick(None, false), Format::Text);
        assert_eq!(Format::pick(Some(Format::Ndjson), false), Format::Ndjson);
        assert_eq!(Format::pick(Some(Format::Text), true), Format::Json, "--json wins");
        assert!(Format::Ndjson.is_json() && !Format::Text.is_json());
    }

    #[test]
    fn envelope() {
        let l = List::new(vec![1, 2, 3]).limit(Some(3));
        assert!(l.truncated, "a full page may have more");
        assert!(!List::new(vec![1]).limit(Some(3)).truncated);
        assert!(List::new(vec![1]).cursor(Some("c".into())).truncated);
        let w = Window { from: "2026-11-01T09:00:00Z".parse().unwrap(), to: "2026-11-01T10:00:00Z".parse().unwrap() };
        let v = serde_json::to_value(List::new(vec!["a"]).limit(Some(20)).window(w)).unwrap();
        assert_eq!(
            v,
            json!({"items": ["a"], "count": 1, "limit": 20, "truncated": false, "next_cursor": null,
                   "window": {"from": "2026-11-01T09:00:00Z", "to": "2026-11-01T10:00:00Z"}})
        );
    }

    #[test]
    fn fields_keep_the_asked_keys_in_order() {
        let items = vec![json!({"a": 1, "b": "x", "c": null}), json!({"a": 2, "d": true})];
        let valid = valid_keys(&items);
        assert_eq!(valid, ["a", "b", "c", "d"]);
        let f = vec!["d".to_string(), "a".to_string()];
        let v = select(items[0].clone(), &f, &valid).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"d":null,"a":1}"#);
        let e = select(items[0].clone(), &["nope".to_string()], &valid).unwrap_err();
        assert_eq!(crate::error::exit_code(&Err(e.context("x"))), 2);
        let e = select(items[0].clone(), &["nope".to_string()], &valid).unwrap_err();
        assert_eq!(crate::error::message(&e), "unknown field \"nope\"; fields: a,b,c,d");
        let v = select(json!({"x": 1}), &f, &[]).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"d":null,"a":null}"#, "nothing to check against");
    }

    #[test]
    fn with_fields_parses_the_flag() {
        let o = out(Format::Json, false).with_fields(Some(" id, title,,"));
        assert_eq!(o.fields, Some(vec!["id".to_string(), "title".to_string()]));
        assert!(!o.text());
        assert_eq!(out(Format::Json, false).with_fields(Some(",")).fields, None);
        assert!(out(Format::Text, false).text() && !out(Format::Text, false).with_fields(Some("a")).text());
    }

    #[test]
    fn selected_text_keeps_the_order() {
        let sel = select(json!({"a": 1, "b": "x"}), &["b".into(), "a".into()], &[]).unwrap();
        assert_eq!(sel.text_line(), "x  1");
    }

    #[test]
    fn text_values() {
        assert_eq!(text_value(&json!("a\x1bb")), "a\\u{1b}b");
        assert_eq!(text_value(&json!(null)), "-");
        assert_eq!(text_value(&json!({"k": [1]})), r#"{"k":[1]}"#);
    }
}
