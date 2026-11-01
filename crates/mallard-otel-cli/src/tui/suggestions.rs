//! What the telemetry filter bars suggest: the keys of each filter language, and where
//! the values come from. The list itself is `mallard_term::tui::suggest`.

use std::time::Instant;

use mallard_otel_client::{Client, api::*};
use mallard_term::tui::{
    Scope,
    suggest::{FETCH_TIMEOUT, Key, LIMIT, Suggest, Values, Want},
};

use super::{
    Embedded, PromptTarget,
    loader::{Data, Kind},
};

/// `op=` asks the aggregate behind `/operations`, which grows with the window (like the
/// web UI, it reads at most this).
const OP_WINDOW: (&str, f64) = ("24h", 86_400.0);

const LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error", "fatal"];

pub(super) const TRACE_KEYS: &[Key] = &[
    Key { text: "service=", values: Values::Fetch },
    Key { text: "op=", values: Values::Fetch },
    Key { text: "errors", values: Values::None },
    Key { text: "min=", values: Values::None },
    Key { text: "max=", values: Values::None },
];

pub(super) const LOG_KEYS: &[Key] = &[
    Key { text: "service=", values: Values::Fetch },
    Key { text: "level=", values: Values::Fixed(LEVELS) },
    Key { text: "trace=", values: Values::None },
];

/// A fetch's answer, back on the UI side. `None`: it failed.
pub(crate) struct Answer {
    target: PromptTarget,
    scope: String,
    want: Want,
    items: Option<Vec<(String, u64)>>,
}

/// The names of the filter language that are not attributes (`query.rs`).
fn is_service(key: &str) -> bool {
    matches!(key, "service" | "svc" | "s")
}

fn is_operation(key: &str) -> bool {
    matches!(key, "op" | "operation" | "name" | "o")
}

fn no_values(key: &str) -> bool {
    matches!(key, "lvl" | "severity" | "sev" | "trace_id" | "min_ms" | "max_ms" | "errors" | "error")
}

/// Keys or values, most frequent first. `None` when it failed or took too long.
async fn fetch(
    c: Client,
    signal: AttributeSignal,
    since: String,
    op_since: String,
    service: Vec<String>,
    want: Want,
) -> Option<Vec<(String, u64)>> {
    let since = Some(since);
    let work = async {
        match want.key.as_deref() {
            Some(k) if is_service(k) => {
                let v = c.services(&ServiceQuery { since, ..Default::default() }).await?;
                Ok(v.into_iter().map(|s| (s.name, s.red.span_count)).collect())
            }
            Some(k) if is_operation(k) && signal == AttributeSignal::Spans => {
                let q = OperationQuery { since: Some(op_since), service, ..Default::default() };
                let v = c.operations(&q).await?;
                Ok(per_name(v.into_iter().map(|o| (o.name, o.red.span_count))))
            }
            key => {
                let q = AttributeQuery {
                    signal,
                    since,
                    service,
                    key: key.map(str::to_string),
                    prefix: (!want.prefix.is_empty()).then(|| want.prefix.clone()),
                    limit: Some(LIMIT),
                    from: None,
                    to: None,
                    metric: None,
                };
                let s = c.attributes(&q).await?;
                Ok(s.items.into_iter().map(|i| (i.value.unwrap_or(i.key), i.count)).collect())
            }
        }
    };
    let result: Result<_, mallard_otel_client::Error> = match tokio::time::timeout(FETCH_TIMEOUT, work).await {
        Ok(r) => r,
        Err(_) => return None,
    };
    result.ok()
}

/// One row per name: the same operation can come from several services and kinds.
fn per_name(rows: impl Iterator<Item = (String, u64)>) -> Vec<(String, u64)> {
    let mut sums = std::collections::HashMap::new();
    for (name, n) in rows {
        *sums.entry(name).or_insert(0) += n;
    }
    let mut out: Vec<(String, u64)> = sums.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

impl Embedded {
    /// The list of the open filter bar, if it has one.
    pub(super) fn suggest_of(&mut self, target: PromptTarget) -> Option<&mut Suggest> {
        match target {
            PromptTarget::TraceFilter => Some(&mut self.trace_suggest),
            PromptTarget::LogFilter => Some(&mut self.log_suggest),
            PromptTarget::MetricSearch | PromptTarget::MetricGroupBy => None,
        }
    }

    /// Start the fetch the open bar's list asks for. Called on each tick, so it never
    /// holds up a key press. A newer fetch aborts the one before.
    pub(super) fn poll_suggest(&mut self) {
        let Some(target) = self.prompt.as_ref().map(|p| p.target) else { return };
        let signal = match target {
            PromptTarget::LogFilter => AttributeSignal::Logs,
            _ => AttributeSignal::Spans,
        };
        let since = self.since().to_string();
        let op_since = if self.window_secs() > OP_WINDOW.1 { OP_WINDOW.0.to_string() } else { since.clone() };
        let service = self.scoped(None);
        let in_app = matches!(self.scope, Scope::App { .. });
        let scope = format!("{since}|{}", service.join(","));
        let now = Instant::now();
        let Some(s) = self.suggest_of(target) else { return };
        s.set_scope(scope.clone());
        let Some(want) = s.poll(now) else { return };
        // What needs no server: the app's own services, and keys without values.
        let local = match want.key.as_deref() {
            Some(k) if is_service(k) && in_app => Some(service.iter().map(|n| (n.clone(), 0)).collect()),
            Some(k) if no_values(k) => Some(vec![]),
            _ => None,
        };
        if let Some(items) = local {
            s.answer(&scope, &want, Some(items), now);
            return;
        }
        let c = self.client.clone();
        let w = want.clone();
        let sc = scope.clone();
        let fut = async move {
            let items = fetch(c, signal, since, op_since, service, w.clone()).await;
            Ok(Answer { target, scope: sc, want: w, items })
        };
        if !self.loader.spawn(Kind::Suggest, fut, |a| Data::Suggest(Box::new(a)))
            && let Some(s) = self.suggest_of(target)
        {
            s.answer(&scope, &want, None, now);
        }
    }

    pub(super) fn suggest_answer(&mut self, a: Answer) {
        if let Some(s) = self.suggest_of(a.target) {
            s.answer(&a.scope, &a.want, a.items, Instant::now());
        }
    }
}
