//! The typed client against a real server over TCP.

use std::time::Duration;

use odl_client::{Client, LogTail, api::*};
use odl_server::{Server, config::Config};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, any_value},
    logs::v1::{LogRecord as OtlpLog, ResourceLogs, ScopeLogs},
    resource::v1::Resource,
};
use prost::Message;
use tokio_util::sync::CancellationToken;

fn logs(n: usize, t0: u64) -> Vec<u8> {
    let recs = (0..n)
        .map(|i| OtlpLog {
            time_unix_nano: t0 + i as u64 * 1_000_000,
            severity_number: if i % 2 == 0 { 9 } else { 17 },
            body: Some(AnyValue {
                value: Some(any_value::Value::StringValue(format!("line {i}"))),
            }),
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".into(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("web".into())),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: recs,
                ..Default::default()
            }],
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
    cfg.ingest.flush_interval = "100ms".into();
    cfg.maintenance.enabled = false;
    cfg.auth.api_tokens = vec!["tok".into()];
    // Pick free ports.
    let free = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string()
    };
    cfg.server.api_addr = free();
    cfg.server.otlp_http_addr = free();
    cfg.server.otlp_grpc_addr = free();
    let api = format!("http://{}", cfg.server.api_addr);
    let otlp = format!("http://{}/v1/logs", cfg.server.otlp_http_addr);
    let server = Server::new(cfg).await.unwrap();
    let shutdown = CancellationToken::new();
    let sd = shutdown.clone();
    let task = tokio::spawn(async move { server.serve(sd).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let c = Client::new(&api, Some("tok".into())).unwrap();
    assert_eq!(c.health().await.unwrap().status, "ok");
    let unauth = Client::new(&api, None).unwrap();
    assert_eq!(unauth.stats().await.unwrap_err().status(), Some(401));

    let t0 = chrono::Utc::now().timestamp_nanos_opt().unwrap() as u64 - 2_000_000_000;
    let http = reqwest::Client::new();
    let r = http
        .post(&otlp)
        .header("content-type", "application/x-protobuf")
        .body(logs(10, t0))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let mut tail = LogTail::new(
        c.clone(),
        LogQuery {
            service: Some("web".into()),
            ..Default::default()
        },
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
    let series = c
        .log_series(&LogSeriesQuery {
            since: Some("10m".into()),
            step: Some("1m".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(series.points.iter().map(|p| p.error).sum::<u64>(), 5);
    let err = c
        .metric_query(&MetricQuery {
            name: "nope".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(404));
    let stats = c.stats().await.unwrap();
    assert_eq!(stats.ingest.signals[1].accepted, 10);

    shutdown.cancel();
    task.await.unwrap();
}
