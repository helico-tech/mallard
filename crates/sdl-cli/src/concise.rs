//! The JSON shape of one event for scripts and agents: the fields that
//! answer "what broke, where, in which request and trace", without the raw
//! Sentry payload (that is several KB; `--raw` adds it).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sdl_client::types::EventDetail;
use serde::Serialize;
use serde_json::Value;

use crate::event_view as ev;

/// In-app frames kept per exception (newest first).
pub const MAX_FRAMES: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConciseEvent {
    pub event_id: String,
    pub issue_id: String,
    pub project_id: u32,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub title: String,
    pub culprit: Option<String>,
    pub message: Option<String>,
    pub platform: String,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    /// The exception chain, outermost first.
    pub exception: Vec<ConciseException>,
    pub request: Option<ConciseRequest>,
    pub user: BTreeMap<String, String>,
    pub tags: BTreeMap<String, String>,
    /// runtime, os, browser, device: one line each.
    pub contexts: BTreeMap<String, String>,
    /// The full Sentry event, only with `--raw`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConciseException {
    #[serde(rename = "type")]
    pub ty: String,
    pub value: Option<String>,
    pub mechanism: Option<String>,
    pub handled: Option<bool>,
    /// Up to [`MAX_FRAMES`] in-app frames, newest first (all frames when
    /// none is in-app).
    pub frames: Vec<ConciseFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConciseFrame {
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub col: Option<u64>,
    pub context_line: Option<String>,
    pub in_app: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConciseRequest {
    pub method: Option<String>,
    pub url: Option<String>,
    pub query: Option<String>,
}

impl ConciseEvent {
    pub fn new(e: EventDetail, raw: bool) -> Self {
        let p = &e.payload;
        let exception = ev::exceptions(p)
            .into_iter()
            .map(|x| {
                let frames = x.frames_newest_first(false);
                let in_app: Vec<_> = frames.iter().filter(|f| f.in_app).collect();
                let pick: Vec<&ev::Frame> = if in_app.is_empty() {
                    frames.iter().take(MAX_FRAMES).copied().collect()
                } else {
                    in_app.into_iter().take(MAX_FRAMES).copied().collect()
                };
                ConciseException {
                    frames: pick
                        .into_iter()
                        .map(|f| ConciseFrame {
                            function: f.function_name().to_string(),
                            file: f.filename.clone(),
                            line: f.lineno,
                            col: f.colno,
                            context_line: f.context_line.as_ref().map(|c| c.trim().to_string()),
                            in_app: f.in_app,
                        })
                        .collect(),
                    ty: x.ty,
                    value: x.value,
                    mechanism: x.mechanism,
                    handled: x.handled,
                }
            })
            .collect();
        let request = ev::request(p).map(|r| ConciseRequest { method: r.method, url: r.url, query: r.query });
        let s = e.summary;
        ConciseEvent {
            message: ev::message(p).or(s.message),
            exception,
            request,
            user: ev::user(p).into_iter().collect(),
            tags: e.tags.into_iter().collect(),
            contexts: ev::contexts(p).into_iter().collect(),
            payload: raw.then_some(e.payload),
            event_id: s.event_id,
            issue_id: s.issue_id,
            project_id: s.project_id,
            timestamp: s.timestamp,
            level: s.level,
            title: s.title,
            culprit: e.culprit,
            platform: s.platform,
            environment: s.environment,
            release: s.release,
            trace_id: s.trace_id,
            span_id: s.span_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdl_client::types::EventSummary;

    fn detail(payload: Value) -> EventDetail {
        EventDetail {
            summary: EventSummary {
                event_id: "e".into(),
                project_id: 1,
                issue_id: "i".into(),
                timestamp: Utc::now(),
                received_at: Utc::now(),
                title: "TypeError: boom".into(),
                level: "error".into(),
                platform: "node".into(),
                environment: None,
                release: Some("web@1".into()),
                user: None,
                message: None,
                trace_id: Some("0af7651916cd43dd8448eb211c80319c".into()),
                span_id: None,
            },
            culprit: Some("handler".into()),
            tags: vec![("url".into(), "/pay".into())],
            payload,
        }
    }

    #[test]
    fn concise_keeps_in_app_frames_and_drops_the_payload() {
        let e = ConciseEvent::new(detail(crate::event_view::tests::node_payload()), false);
        assert!(e.payload.is_none());
        let x = &e.exception[0];
        assert!(!x.frames.is_empty() && x.frames.len() <= MAX_FRAMES);
        assert!(x.frames.iter().all(|f| f.in_app), "{:?}", x.frames);
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("payload").is_none());
        assert_eq!(v["exception"][0]["type"], x.ty.as_str());
        assert_eq!(v["tags"]["url"], "/pay");
        assert_eq!(v["trace_id"], "0af7651916cd43dd8448eb211c80319c");
    }

    #[test]
    fn raw_keeps_the_payload() {
        let e = ConciseEvent::new(detail(serde_json::json!({"a": 1})), true);
        assert_eq!(e.payload, Some(serde_json::json!({"a": 1})));
        assert!(e.exception.is_empty() && e.request.is_none());
    }
}
