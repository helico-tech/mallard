//! `sdl-loadgen`: push realistic Sentry error envelopes at a server and
//! measure throughput, latency and query times.
//!
//! ```text
//! sdl-loadgen --url http://localhost:8000 --token dev ingest --rate 500 --duration 60s
//! sdl-loadgen --url http://localhost:8000 --token dev query --project loadtest
//! ```

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hdrhistogram::Histogram;
use rand::RngExt;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "sdl-loadgen", about = "Load generator for sentry-ducklake")]
struct Cli {
    /// Server base URL.
    #[arg(long, env = "SDL_URL", default_value = "http://localhost:8000")]
    url: String,
    /// API token (to create the project and run queries).
    #[arg(long, env = "SDL_TOKEN")]
    token: String,
    /// Project slug to use (created if missing).
    #[arg(long, default_value = "loadtest")]
    project: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send events.
    Ingest {
        /// Target events per second (0 = as fast as possible).
        #[arg(long, default_value_t = 200)]
        rate: u64,
        /// How long to run, e.g. `60s`.
        #[arg(long, value_parser = mallard_common::duration::parse, default_value = "30s")]
        duration: Duration,
        /// Stop after this many events (0 = no limit).
        #[arg(long, default_value_t = 0)]
        events: u64,
        /// Parallel HTTP connections.
        #[arg(long, default_value_t = 64)]
        concurrency: usize,
        /// Distinct issues to spread events over (Zipf-like: a few issues get most events).
        #[arg(long, default_value_t = 200)]
        issues: u32,
        /// Distinct users.
        #[arg(long, default_value_t = 5000)]
        users: u32,
        /// Breadcrumbs per event (payload size: ~25 bytes JSON each... and more).
        #[arg(long, default_value_t = 30)]
        breadcrumbs: usize,
        /// Gzip request bodies like the Node/Python SDKs do.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        gzip: bool,
    },
    /// Time the query API.
    Query {
        /// Repetitions per query.
        #[arg(long, default_value_t = 20)]
        reps: usize,
    },
}

const FUNCS: &[&str] = &[
    "handleClick",
    "submitForm",
    "loadDashboard",
    "renderChart",
    "fetchUser",
    "parseResponse",
    "validateInput",
    "computeTotals",
    "applyDiscount",
    "syncCart",
    "openModal",
    "formatDate",
    "resolveRoute",
    "hydrate",
];
const FILES: &[&str] = &[
    "src/components/Checkout.tsx",
    "src/api/client.ts",
    "src/pages/Dashboard.tsx",
    "src/lib/money.ts",
    "src/hooks/useCart.ts",
    "src/router.ts",
    "src/utils/date.ts",
    "src/store/session.ts",
];
const TYPES: &[&str] = &["TypeError", "ReferenceError", "RangeError", "SyntaxError", "Error", "ChunkLoadError"];
const BROWSERS: &[&str] = &[
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36",
];

/// Pick an issue number with a skewed distribution (rank ~ 1/x).
fn skewed(rng: &mut impl rand::Rng, n: u32) -> u32 {
    let u: f64 = rng.random_range(0.0..1.0);
    ((n as f64).powf(u) - 1.0).floor() as u32 % n.max(1)
}

fn make_event(rng: &mut impl rand::Rng, issues: u32, users: u32, breadcrumbs: usize) -> Value {
    let issue = skewed(rng, issues);
    // The issue number fully determines type + frames, so grouping is stable.
    let ty = TYPES[(issue as usize) % TYPES.len()];
    let depth = 8 + (issue as usize % 8);
    let frames: Vec<Value> = (0..depth)
        .map(|i| {
            let f = FUNCS[(issue as usize * 7 + i * 3) % FUNCS.len()];
            let file = FILES[(issue as usize + i) % FILES.len()];
            json!({
                "function": format!("{f}{}", if i == depth - 1 { issue.to_string() } else { String::new() }),
                "filename": format!("/{file}"),
                "abs_path": format!("https://app.example.com/{file}"),
                "lineno": 10 + (i * 13) % 300, "colno": 5 + i, "in_app": i > 1
            })
        })
        .collect();
    let crumbs: Vec<Value> = (0..breadcrumbs)
        .map(|i| {
            json!({"timestamp": chrono::Utc::now().timestamp() as f64 - i as f64, "category": "ui.click",
            "message": format!("button#action-{} clicked", rng.random_range(0..50))})
        })
        .collect();
    let user = rng.random_range(0..users);
    json!({
        "event_id": uuid::Uuid::new_v4().simple().to_string(),
        "timestamp": chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
        "platform": "javascript",
        "level": if issue.is_multiple_of(10) { "warning" } else { "error" },
        "release": format!("web@1.{}.0", rng.random_range(0..5)),
        "environment": if rng.random_range(0..10) == 0 { "staging" } else { "production" },
        "transaction": format!("/route/{}", issue % 20),
        "user": {"id": format!("user-{user}"), "ip_address": "{{auto}}"},
        "tags": {"tenant": format!("t{}", user % 40), "feature_flag": if user % 2 == 0 { "on" } else { "off" }},
        "request": {"url": format!("https://app.example.com/route/{}", issue % 20),
            "headers": {"User-Agent": BROWSERS[rng.random_range(0..BROWSERS.len())]}},
        "contexts": {"trace": {"trace_id": uuid::Uuid::new_v4().simple().to_string(), "span_id": format!("{:016x}", rng.random::<u64>())}},
        "exception": {"values": [{"type": ty, "value": format!("{} #{} failed for id {}", ty, issue, rng.random_range(0..100000)),
            "mechanism": {"type": "onerror", "handled": false}, "stacktrace": {"frames": frames}}]},
        "breadcrumbs": {"values": crumbs},
        "sdk": {"name": "sentry.javascript.browser", "version": "11.10.0"}
    })
}

fn envelope(ev: &Value, gzip: bool) -> Vec<u8> {
    let payload = ev.to_string();
    let body = format!(
        "{{\"event_id\":\"{}\"}}\n{{\"type\":\"event\",\"length\":{}}}\n{payload}\n",
        ev["event_id"].as_str().unwrap(),
        payload.len()
    );
    if !gzip {
        return body.into_bytes();
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(body.as_bytes()).unwrap();
    enc.finish().unwrap()
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    ok: AtomicU64,
    throttled: AtomicU64,
    failed: AtomicU64,
    bytes: AtomicU64,
}

async fn ensure_project(c: &sdl_client::Client, slug: &str) -> Result<sdl_client::types::Project> {
    match c.project(slug).await {
        Ok(p) => Ok(p),
        Err(e) if e.is_not_found() => Ok(c
            .create_project(&sdl_client::types::CreateProject {
                name: slug.into(),
                slug: Some(slug.into()),
                platform: Some("javascript".into()),
            })
            .await?),
        Err(e) => Err(e.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn ingest(
    cli: &Cli,
    rate: u64,
    duration: Duration,
    max_events: u64,
    concurrency: usize,
    issues: u32,
    users: u32,
    breadcrumbs: usize,
    gzip: bool,
) -> Result<()> {
    let client = sdl_client::Client::new(&cli.url, Some(&cli.token))?;
    let project = ensure_project(&client, &cli.project).await?;
    let url = format!("{}/api/{}/envelope/", cli.url.trim_end_matches('/'), project.id);
    let auth = format!("Sentry sentry_version=7, sentry_client=sdl-loadgen/0.1, sentry_key={}", project.public_key);
    let http = reqwest::Client::builder().pool_max_idle_per_host(concurrency).build()?;
    let counters = Arc::new(Counters::default());
    let hist = Arc::new(Mutex::new(Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)?));
    let started = Instant::now();
    let deadline = started + duration;
    let interval = (rate > 0).then(|| Duration::from_secs_f64(concurrency as f64 / rate as f64));
    let budget = Arc::new(AtomicU64::new(if max_events == 0 { u64::MAX } else { max_events }));

    let mut tasks = Vec::new();
    for w in 0..concurrency {
        let (http, url, auth, counters, hist, budget) =
            (http.clone(), url.clone(), auth.clone(), counters.clone(), hist.clone(), budget.clone());
        tasks.push(tokio::spawn(async move {
            // Stagger workers so a paced run doesn't send in bursts.
            // Ramp up over ~2s (macOS' default listen backlog is 128) and
            // stagger paced workers so they don't send in bursts.
            let ramp = interval.unwrap_or(Duration::from_secs(2)).min(Duration::from_secs(2));
            tokio::time::sleep(ramp.mul_f64(w as f64 / concurrency as f64)).await;
            let mut next = Instant::now();
            let mut local = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
            while Instant::now() < deadline {
                if budget.try_update(Ordering::SeqCst, Ordering::SeqCst, |b| b.checked_sub(1)).is_err() {
                    break;
                }
                let body = {
                    let mut rng = rand::rng();
                    envelope(&make_event(&mut rng, issues, users, breadcrumbs), gzip)
                };
                counters.bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
                let mut req = http
                    .post(&url)
                    .header("x-sentry-auth", &auth)
                    .header("content-type", "application/x-sentry-envelope");
                if gzip {
                    req = req.header("content-encoding", "gzip");
                }
                let t0 = Instant::now();
                counters.sent.fetch_add(1, Ordering::Relaxed);
                match req.body(body).send().await {
                    Ok(r) if r.status().is_success() => {
                        counters.ok.fetch_add(1, Ordering::Relaxed);
                        let _ = local.record(t0.elapsed().as_micros() as u64);
                    }
                    Ok(r) if r.status().as_u16() == 429 => {
                        counters.throttled.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Ok(r) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        eprintln!("HTTP {}: {}", r.status(), r.text().await.unwrap_or_default());
                    }
                    Err(e) => {
                        counters.failed.fetch_add(1, Ordering::Relaxed);
                        eprintln!("request error: {e}");
                    }
                }
                if let Some(iv) = interval {
                    next += iv;
                    let now = Instant::now();
                    if next > now {
                        tokio::time::sleep(next - now).await;
                    } else {
                        next = now;
                    }
                }
            }
            hist.lock().await.add(&local).unwrap();
        }));
    }
    let progress = {
        let counters = counters.clone();
        tokio::spawn(async move {
            let mut last = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let ok = counters.ok.load(Ordering::Relaxed);
                eprintln!("  {:>6.0}s  ok={ok}  ({}/s)", started.elapsed().as_secs_f64(), (ok - last) / 5);
                last = ok;
            }
        })
    };
    for t in tasks {
        t.await?;
    }
    progress.abort();
    let secs = started.elapsed().as_secs_f64();
    let h = hist.lock().await;
    let ok = counters.ok.load(Ordering::Relaxed);
    println!("events sent      {}", counters.sent.load(Ordering::Relaxed));
    println!("accepted (200)   {ok}");
    println!("throttled (429)  {}", counters.throttled.load(Ordering::Relaxed));
    println!("failed           {}", counters.failed.load(Ordering::Relaxed));
    println!("duration         {secs:.1}s");
    println!("throughput       {:.0} events/s", ok as f64 / secs);
    println!(
        "avg body         {} bytes (gzip={gzip})",
        counters.bytes.load(Ordering::Relaxed) / counters.sent.load(Ordering::Relaxed).max(1)
    );
    println!(
        "latency ms       p50 {:.1}  p95 {:.1}  p99 {:.1}  max {:.1}",
        h.value_at_quantile(0.50) as f64 / 1000.0,
        h.value_at_quantile(0.95) as f64 / 1000.0,
        h.value_at_quantile(0.99) as f64 / 1000.0,
        h.max() as f64 / 1000.0
    );
    if counters.failed.load(Ordering::Relaxed) > 0 {
        bail!("some requests failed");
    }
    Ok(())
}

async fn timed<T, F: std::future::Future<Output = Result<T, sdl_client::Error>>>(
    name: &str,
    reps: usize,
    mut f: impl FnMut() -> F,
) -> Result<T> {
    let mut times = Vec::with_capacity(reps);
    let mut last = None;
    for _ in 0..reps {
        let t0 = Instant::now();
        last = Some(f().await.with_context(|| name.to_string())?);
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| times[((times.len() as f64 - 1.0) * q).round() as usize];
    println!("{name:<38} p50 {:>7.1} ms   p95 {:>7.1} ms", p(0.5), p(0.95));
    Ok(last.unwrap())
}

async fn query(cli: &Cli, reps: usize) -> Result<()> {
    use sdl_client::types::*;
    let c = sdl_client::Client::new(&cli.url, Some(&cli.token))?;
    let p = cli.project.as_str();
    let stats = c.stats(p, &WindowQuery { since: Some("30d".into()), interval: None }).await?;
    println!("project {p}: {} events in 30d, {} issues\n", stats.total_events, stats.unique_issues);
    let q24 = IssueQuery { since: Some("24h".into()), ..Default::default() };
    let list = timed("issues list (24h, unresolved)", reps, || c.issues(p, &q24)).await?;
    let q14 = IssueQuery { since: Some("14d".into()), ..Default::default() };
    timed("issues list (14d)", reps, || c.issues(p, &q14)).await?;
    let qs = IssueQuery {
        since: Some("14d".into()),
        query: Some("TypeError".into()),
        sort: Some("count".into()),
        ..Default::default()
    };
    timed("issues search + sort by count (14d)", reps, || c.issues(p, &qs)).await?;
    let top = list.issues.first().context("no issues")?;
    let w = WindowQuery { since: Some("14d".into()), interval: None };
    let detail = timed("issue detail + tag facets (top issue)", reps, || c.issue(p, &top.id, &w)).await?;
    let eq = EventQuery { issue: Some(top.id.clone()), limit: Some(50), ..Default::default() };
    timed("events of issue (50 newest)", reps, || c.events(p, &eq)).await?;
    let ev = detail.latest_event_id.context("no event")?;
    timed("event detail (with issue hint)", reps, || c.event(p, &ev, Some(&top.id))).await?;
    timed("event detail (no hint)", reps.min(5), || c.event(p, &ev, None)).await?;
    let w24 = WindowQuery { since: Some("24h".into()), interval: Some("1h".into()) };
    timed("stats (24h, hourly)", reps, || c.stats(p, &w24)).await?;
    let w30 = WindowQuery { since: Some("30d".into()), interval: Some("1d".into()) };
    timed("stats (30d, daily)", reps, || c.stats(p, &w30)).await?;
    let trace = c.event(p, &ev, Some(&top.id)).await?.summary.trace_id.context("no trace id")?;
    timed("events by trace id (30d)", reps, || c.events_by_trace(&trace)).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ingest { rate, duration, events, concurrency, issues, users, breadcrumbs, gzip } => {
            ingest(&cli, rate, duration, events, concurrency, issues, users, breadcrumbs, gzip).await
        }
        Cmd::Query { reps } => query(&cli, reps).await,
    }
}
