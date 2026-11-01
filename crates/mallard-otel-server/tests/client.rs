//! The typed client against a real server over TCP.

use std::{sync::Arc, time::Duration};

use mallard_otel_client::{Client, LogTail, api::*};
use mallard_otel_server::{Server, config::Config};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value},
    logs::v1::{LogRecord as OtlpLog, ResourceLogs, ScopeLogs},
    resource::v1::Resource,
};
use prost::Message;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Serve `app` on `listener` until `shutdown`.
fn serve(listener: TcpListener, app: axum::Router, shutdown: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await.unwrap();
    })
}

fn logs(n: usize, t0: u64) -> Vec<u8> {
    let recs = (0..n)
        .map(|i| OtlpLog {
            time_unix_nano: t0 + i as u64 * 1_000_000,
            severity_number: if i % 2 == 0 { 9 } else { 17 },
            body: Some(AnyValue { value: Some(any_value::Value::StringValue(format!("line {i}"))) }),
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".into(),
                    value: Some(AnyValue { value: Some(any_value::Value::StringValue("web".into())) }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs { log_records: recs, ..Default::default() }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.storage.catalog = dir.path().join("c.ducklake").to_string_lossy().into();
    cfg.storage.data_path = format!("{}/", dir.path().join("lake").to_string_lossy());
    cfg.ingest.flush_interval = Duration::from_millis(100);
    cfg.maintenance.enabled = false;
    cfg.auth.api_tokens = vec!["tok".into()];
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let otlp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", api_listener.local_addr().unwrap());
    let otlp = format!("http://{}/v1/logs", otlp_listener.local_addr().unwrap());
    let server = Arc::new(Server::new(cfg).await.unwrap());
    let shutdown = CancellationToken::new();
    let api_task = serve(api_listener, server.api_router(), shutdown.clone());
    let otlp_task = serve(otlp_listener, server.otlp_http_router(), shutdown.clone());
    let sd = shutdown.clone();
    let bg = tokio::spawn(async move { server.run_background(sd.cancelled_owned()).await.unwrap() });

    let c = Client::new(&api, Some("tok")).unwrap();
    assert_eq!(c.health().await.unwrap().status, "ok");
    let unauth = Client::new(&api, None).unwrap();
    assert_eq!(unauth.stats().await.unwrap_err().status(), Some(401));

    let t0 = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64 - 2_000_000_000;
    let http = reqwest::Client::new();
    let r = http.post(&otlp).header("content-type", "application/x-protobuf").body(logs(10, t0)).send().await.unwrap();
    assert_eq!(r.status(), 200);

    let mut tail = LogTail::new(
        c.clone(),
        LogQuery { service: vec!["web".into()], ..Default::default() },
        chrono::Utc::now() - chrono::TimeDelta::minutes(5),
    );
    let mut got = Vec::new();
    for _ in 0..50 {
        got.extend(tail.poll().await.unwrap());
        if got.len() >= 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(got.len(), 10);
    assert_eq!(got[0].body, "line 0", "tail returns oldest first");
    assert!(tail.poll().await.unwrap().is_empty(), "no duplicates");

    // Query params with typed values (bool, u8, DateTime) survive URL encoding.
    let errs = c
        .logs(&LogQuery {
            from: Some(chrono::Utc::now() - chrono::TimeDelta::minutes(10)),
            min_severity: Some(17),
            limit: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(errs.len(), 3);
    // Several service values: the client sends them all (ADR 0028).
    let of = |service: &[&str]| LogQuery {
        since: Some("10m".into()),
        service: service.iter().map(|s| s.to_string()).collect(),
        limit: Some(1),
        ..Default::default()
    };
    assert_eq!(c.logs(&of(&["web", "nope"])).await.unwrap().len(), 1, "any of them");
    assert_eq!(c.logs(&of(&["nope", "also-not"])).await.unwrap().len(), 0);
    assert_eq!(c.logs_page(&of(&["nope", "web"])).await.unwrap().0.len(), 1);
    // Pages over inlined rows (the default inlining keeps these in the catalog).
    let mut q = LogQuery { since: Some("10m".into()), limit: Some(3), ..Default::default() };
    let mut seen = Vec::new();
    loop {
        let (page, next) = c.logs_page(&q).await.unwrap();
        seen.extend(page.into_iter().map(|r| r.body));
        match next {
            Some(cur) => q.cursor = Some(cur),
            None => break,
        }
    }
    let want: Vec<String> = (0..10).rev().map(|i| format!("line {i}")).collect();
    assert_eq!(seen, want, "newest first, each once");
    let span = c.span("0000000000000001", &TraceGetQuery::default()).await.unwrap_err();
    assert_eq!(span.status(), Some(404));
    assert!(matches!(
        c.span("xyz", &TraceGetQuery::default()).await,
        Err(mallard_otel_client::Error::InvalidSpanId(_))
    ));
    let schema = c.schema().await.unwrap();
    assert_eq!(schema.tables[1].name, "logs");
    let keys = c
        .attributes(&AttributeQuery {
            signal: AttributeSignal::Logs,
            from: None,
            to: None,
            since: Some("10m".into()),
            service: vec![],
            metric: None,
            key: None,
            prefix: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!((keys.total_rows, keys.items[0].key.as_str()), (10, "service.name"));
    let series = c
        .log_series(&LogSeriesQuery { since: Some("10m".into()), step: Some("1m".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(series.points.iter().map(|p| p.error).sum::<u64>(), 5);
    let err = c.metric_query(&MetricQuery { name: "nope".into(), ..Default::default() }).await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    let stats = c.stats().await.unwrap();
    assert_eq!(stats.ingest.signals[1].accepted, 10);

    shutdown.cancel();
    api_task.await.unwrap();
    otlp_task.await.unwrap();
    bg.await.unwrap();
}
