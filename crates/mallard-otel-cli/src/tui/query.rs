//! Tiny filter language used by the trace and log filter bars.
//!
//! Tokens are separated by spaces; values may be quoted:
//! - traces: `service=checkout op="GET /cart" errors min=250ms max=2s http.route=/cart`
//! - logs:   `service=api level=warn trace=0af7... payment failed`
//!
//! Unknown `key=value` tokens become attribute filters. Bare words are the
//! text search (logs) or, for traces, `errors`/`err`.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraceFilter {
    pub service: Option<String>,
    pub operation: Option<String>,
    pub errors_only: bool,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub attrs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogFilter {
    pub service: Option<String>,
    pub level: Option<u8>,
    pub trace: Option<String>,
    pub text: Option<String>,
    pub attrs: Vec<String>,
}

pub use mallard_term::tui::suggest::tokenize;

/// `250` (ms), `250ms`, `1.5s`, `2m`, `100us`.
pub fn parse_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (n, unit) = s.split_at(idx);
    let n: f64 = n.parse().ok()?;
    let mult = match unit {
        "" | "ms" => 1.0,
        "us" | "µs" => 0.001,
        "s" => 1000.0,
        "m" => 60_000.0,
        _ => return None,
    };
    Some(n * mult)
}

pub fn parse_trace_filter(s: &str) -> Result<TraceFilter, String> {
    let mut f = TraceFilter::default();
    for tok in tokenize(s) {
        match tok.split_once('=') {
            Some((k, v)) => match k {
                "service" | "svc" | "s" => f.service = Some(v.to_string()),
                "op" | "operation" | "name" | "o" => f.operation = Some(v.to_string()),
                "min" | "min_ms" => f.min_ms = Some(parse_ms(v).ok_or(format!("bad duration {v:?}"))?),
                "max" | "max_ms" => f.max_ms = Some(parse_ms(v).ok_or(format!("bad duration {v:?}"))?),
                "errors" | "error" => f.errors_only = matches!(v, "1" | "true" | "yes" | "on"),
                "" => return Err(format!("missing key in {tok:?}")),
                _ => f.attrs.push(format!("{k}={v}")),
            },
            None => match tok.to_ascii_lowercase().as_str() {
                "errors" | "error" | "err" => f.errors_only = true,
                t if t.starts_with('>') => f.min_ms = Some(parse_ms(&t[1..]).ok_or(format!("bad duration {tok:?}"))?),
                _ => {
                    return Err(format!("unknown filter {tok:?} (try service=, op=, errors, min=, key=value)"));
                }
            },
        }
    }
    Ok(f)
}

pub fn parse_log_filter(s: &str) -> Result<LogFilter, String> {
    let mut f = LogFilter::default();
    let mut words = Vec::new();
    for tok in tokenize(s) {
        match tok.split_once('=') {
            Some((k, v)) if !k.is_empty() => match k {
                "service" | "svc" | "s" => f.service = Some(v.to_string()),
                "level" | "lvl" | "severity" | "sev" => {
                    f.level = Some(crate::severity::parse(v).ok_or(format!("unknown level {v:?}"))?)
                }
                "trace" | "trace_id" => f.trace = Some(v.to_string()),
                _ => f.attrs.push(format!("{k}={v}")),
            },
            _ => words.push(tok),
        }
    }
    if !words.is_empty() {
        f.text = Some(words.join(" "));
    }
    Ok(f)
}

impl TraceFilter {
    /// Render back to the filter language.
    pub fn to_query(&self) -> String {
        let mut parts = Vec::new();
        let q = |v: &str| {
            if v.contains(char::is_whitespace) { format!("\"{v}\"") } else { v.to_string() }
        };
        if let Some(s) = &self.service {
            parts.push(format!("service={}", q(s)));
        }
        if let Some(o) = &self.operation {
            parts.push(format!("op={}", q(o)));
        }
        if self.errors_only {
            parts.push("errors".into());
        }
        if let Some(m) = self.min_ms {
            parts.push(format!("min={m}ms"));
        }
        if let Some(m) = self.max_ms {
            parts.push(format!("max={m}ms"));
        }
        for a in &self.attrs {
            parts.push(a.clone());
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_quotes() {
        assert_eq!(tokenize(r#"op="GET /cart" errors"#), vec!["op=GET /cart", "errors"]);
        assert_eq!(tokenize("  a   b "), vec!["a", "b"]);
    }

    #[test]
    fn trace_filter() {
        let f = parse_trace_filter(r#"service=checkout op="GET /x" errors min=1.5s http.route=/x"#).unwrap();
        assert_eq!(f.service.as_deref(), Some("checkout"));
        assert_eq!(f.operation.as_deref(), Some("GET /x"));
        assert!(f.errors_only);
        assert_eq!(f.min_ms, Some(1500.0));
        assert_eq!(f.attrs, vec!["http.route=/x"]);
        assert_eq!(parse_trace_filter(&f.to_query()).unwrap(), f);
        // A name as the server has it can hold any white space, and must stay one word.
        let raw = parse_trace_filter("service=\"a\nb\tc\"").unwrap();
        assert_eq!(parse_trace_filter(&raw.to_query()).unwrap(), raw);
        assert!(parse_trace_filter("bogus").is_err());
        assert_eq!(parse_trace_filter(">200").unwrap().min_ms, Some(200.0));
    }

    #[test]
    fn log_filter() {
        let f = parse_log_filter("service=api level=warn payment failed k=v").unwrap();
        assert_eq!(f.service.as_deref(), Some("api"));
        assert_eq!(f.level, Some(13));
        assert_eq!(f.text.as_deref(), Some("payment failed"));
        assert_eq!(f.attrs, vec!["k=v"]);
        assert!(parse_log_filter("level=nope").is_err());
        assert_eq!(parse_log_filter("").unwrap(), LogFilter::default());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_ms("250"), Some(250.0));
        assert_eq!(parse_ms("2m"), Some(120_000.0));
        assert_eq!(parse_ms("x"), None);
    }
}
