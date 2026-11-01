# Review: `crates/odl-cli` + `crates/odl-client`

Scope: every file in both crates (~6.2k lines) was read. Paths are relative to the repo root.
I also ran `cargo clippy -W clippy::pedantic` on both crates. It reports about 230 warnings. Most are numeric `as` casts and missing `# Errors`/`#[must_use]` docs, and none of them is a bug. Default clippy is clean. The pedantic noise is not listed below unless it matters.

Severity scale: **bug** = wrong behaviour today, **high** = security, performance or design problem with a real effect, **medium** = API or maintainability problem worth fixing soon, **low** = cleanup.

---

1. **bug** — `crates/odl-client/src/lib.rs:221` together with `crates/odl-cli/src/tui/views/logs.rs:155-162`
   `LogTail::poll` sets `truncated = records.len() >= limit`. The poll window is the time since the last poll plus 31 s of look-back, so the page fills with records that were already seen. Any stream above about 15 records/s (TUI limit 500, window about 33 s) reports "truncated" on every poll. The TUI then inserts a "… older records skipped …" row every 2 s, even though nothing was lost.
   *Fix:* report truncation only when the page is full **and** the response does not overlap what was already seen. After `records.reverse()`, before updating `seen`, use `self.truncated = full && records.first().is_some_and(|r| !self.seen.contains_key(&key(r)))`. Add a unit test for it (see 29).

2. **bug** — `crates/odl-cli/src/tui/views/metrics.rs:161-168`, `crates/odl-cli/src/tui/views/overview.rs:132-140`, root cause in `crates/odl-cli/src/tui/loader.rs:116-118`
   `Loader::load(kind, force=false, …)` returns `true` without starting anything when a request of that kind is still running, and the caller then stamps `refreshed = now`.
   - Changing the aggregation (`a`/`A`) only clears `c.refreshed`, so it is affected. Press `a` twice quickly, or once on a slow server: the old aggregation's result arrives, `set_result` accepts it because only the name is matched, and it is drawn under the new aggregation's label and formatter. The new query is not sent until the 10 s refresh comes round.
   - `show_traces_for` has the same flaw for `Kind::Traces`: a pending list for the old filter is applied.
   *Fix:* call `self.loader.cancel(kind)` wherever query parameters change, as `set_metric_group_by` and `set_trace_filter` already do. A sturdier option is to make `load` take a request key (for example the serialized query) and skip only when the pending request has the same key. Also match `set_result` on the aggregation and group-by, not only the name.

3. **high (security)** — `crates/odl-cli/src/fmt.rs:43-51, 340-350, 265-309, 419-436`
   On a terminal, the CLI prints log bodies, attributes, service names, span names and SQL cells exactly as received. Any app that can send OTLP data can therefore put escape sequences on the operator's terminal: CSI, OSC 52 (write to the clipboard), OSC 8 (links), window-title changes. `strip_ansi` only runs when stdout is *not* a TTY, and it only knows CSI, so OSC sequences also pass through into pipes. The TUI is safe, because ratatui drops control characters in `Span::styled_graphemes` and `Buffer::set_stringn`.
   *Fix:* clean every value that comes from the server **before** styling it, whether or not colour is on. For example, `fn clean(s: &str) -> Cow<'_, str>` replaces `char::is_control` characters (except `\n` where wanted) with `\u{FFFD}` or an escaped form like `\x1b`. Use it in `print_log`, `trace_lines`, `print_sql` and the table cells.

4. **high (performance)** — `crates/odl-cli/src/tui/views/logs.rs:420-452`, `crates/odl-cli/src/tui/views/traces.rs:457-485`
   `draw` rebuilds a `Row` for **every** record on every frame: up to 5000 log records, each with a local-time conversion, a `format!`, `trunc(body, 400)` and several `String` clones. The trace waterfall does the same for every span, and traces can have thousands. `draw` runs 4 times a second plus once per key or resize. The service-column width at `logs.rs:452` also scans all records on every frame.
   *Fix:* call `fit_offset` first, then build rows only for `records[offset..offset + height]`. Render them with a `TableState` whose offset is 0 and whose selection is shifted by the offset, or use `Table::rows` on the slice. Alternatively, cache the formatted cells when records are added.

5. **high** — `crates/odl-cli/src/tui/loader.rs:99, 153-158`, `crates/odl-cli/src/tui/views/traces.rs:80`, `crates/odl-cli/src/lib.rs:280-289`
   The loader turns `odl_client::Error` into a `String`. Callers then detect conditions by searching that text: `e.contains("max_window")` in the TUI, and `message.contains("max_window")` in the CLI. Both depend on the wording of a human-readable server message (`odl-server/src/query/mod.rs:202`), and the logic is written twice. Both fallbacks hard-code `24h`, which fails if the server's `max_window` is smaller. The CLI also narrows 7d to 24h without telling the user.
   *Fix:* keep `Result<Data, odl_client::Error>` in `Loaded` and turn it into text only when displaying it. Add a typed check on the client, such as `Error::is_window_too_large()`, keyed on a stable `ApiError.error` code. This needs the server to send a dedicated code such as `"window_too_large"`, which is outside this scope. Print a note to stderr when the CLI falls back.

6. **medium (bug)** — `crates/odl-cli/src/tui/input.rs:53-57`, `crates/odl-cli/src/tui/mod.rs:399-406`
   While a prompt is open, every key goes to `Input::handle`, and `KeyCode::Char(c)` ignores modifiers. So Ctrl-S inserts "s", Alt-x inserts "x", and host shortcuts other than F1/F2 never reach the host. This contradicts the module doc (`mod.rs:9-11`). Separately, `Input`'s Ctrl-C → Cancel branch (`input.rs:33`) can never run, because `handle_event` turns Ctrl-C into `Quit` first (`mod.rs:392`).
   *Fix:* use `KeyCode::Char(c) if !k.modifiers.intersects(CONTROL | ALT)`. Add `InputResult::Ignored` for keys the input does not use, and return `Outcome::Ignored` for them. Then pick one meaning for Ctrl-C in a prompt (cancel is the common one) and delete the branch that cannot run.

7. **medium** — `crates/odl-cli/src/fmt.rs:43-51`
   `emit` is library code: `mallard` calls it through `odl_cli::run`. It calls `std::process::exit(0)` on `BrokenPipe` and silently drops every other write error, for example a full disk when output is redirected. `docs/architecture.md` says library crates have no global side effects.
   *Fix:* make `emit` (and `outln!`) return `io::Result<()>`, pass it up from `run`, and let the binary turn `BrokenPipe` into exit code 0.

8. **medium** — `crates/odl-client/src/lib.rs:204-211, 239`
   `last_poll` changes only after a successful poll, and the TUI stops polling while paused. The next window is therefore "time since the last success". After a long pause or outage this asks for `since=<hours>`. Once the gap passes the server's `query.max_window` (default 31d), every poll fails with 400 and the tail never recovers.
   *Fix:* cap the window, for example at `max(lookback, 10 min)`, and set `truncated = true` when the cap applies. A gap row is the right signal there.

9. **medium** — `crates/odl-client/src/lib.rs:17-25, 80-84`
   - `Error` is a public enum without `#[non_exhaustive]`, so adding a variant breaks users.
   - `#[error("request failed: {0}")] Http(#[from] reqwest::Error)` puts the source error inside the message, and `#[from]` also exposes it as `source()`. `anyhow`'s `{e:#}` (`main.rs:32`) therefore prints the reqwest message twice.
   - A non-JSON error body becomes `message` in full, so a proxy's 502 HTML page is printed or flashed whole.
   - `BadUrl` does not say what a valid URL looks like.

   *Fix:* add `#[non_exhaustive]` and use `#[error("request failed")] Http(#[from] …)`. Cut fallback bodies to the first line or about 200 characters. Use `#[error("invalid base URL {0:?}: must start with http:// or https://")]`.

10. **medium** — `crates/odl-cli/src/lib.rs:15`, `crates/odl-cli/src/lib.rs:169`, `crates/odl-cli/src/tui/loader.rs:70`
    The public surface is larger than needed. `pub mod fmt` exposes about 20 helpers (`color`, `strip_ansi`, `emit`, `compact`, `ms`, `bytes`, `sparkline`, `severity_label`, `trace_lines`, every `print_*`). Nothing outside the crate uses them except the `odl` binary's `paint_err`. `Win::since_secs` is `pub` but only used inside the crate. `loader::STARTED` is a `pub static`.
    *Fix:* make it `pub(crate) mod fmt` and add `#[doc(hidden)] pub use fmt::paint_err;` for the binary, or give the binary its own small helper. Make `since_secs` `pub(crate)` and remove `STARTED` (see 22).

11. **medium** — `crates/odl-cli/src/lib.rs:26, 32, 46, 203`, `crates/odl-cli/src/tui/mod.rs:93`, `crates/odl-client/src/lib.rs:40, 143`
    Public types `Output`, `Win`, `Cmd`, `Ctx`, `Embedded`, `Client` and `LogTail` have no `Debug`, which the Rust API Guidelines call C-DEBUG. `Output` could also derive `Debug, Default` (Table) and `Hash`.
    *Fix:* derive `Debug` where it is safe. For `Client`, write `Debug` by hand and hide the token (`token: token.as_ref().map(|_| "***")`).

12. **medium** — duplicated logic that has already drifted apart:
    - Severity parsing: `crates/odl-cli/src/lib.rs:182-195` and `tui/query.rs:75-85`. Only the TUI accepts `err`.
    - Severity labels: `crates/odl-cli/src/fmt.rs:315-338` and `tui/theme.rs:107-123`. The CLI shows `FATAL` for 25 and up and the raw text for 0; the TUI shows `-` for 25 and up and maps the text for 0.
    - Uptime formatting: `fmt.rs:189-196` and `tui/views/status.rs:17-24`.
    - Attribute joining: `lib.rs:178` and `views/logs.rs:144`, `views/traces.rs:178`.
    - The `max_window` fallback (see 5).
    - min/max folds: `fmt.rs:379, 405` and `views/metrics.rs:382`.

    *Fix:* add one small `severity` module (`parse` plus `label` returning name and level) used by both the CLI and the TUI. Reuse `attrs()`, and add a shared `min_max(&[f64])` helper.

13. **medium** — `crates/odl-cli/src/tui/mod.rs:188-210, 684-697`, `crates/odl-cli/src/lib.rs:324-327`
    `set_since` / `with_since` accept any string. An invalid one is added to `windows`, and the server rejects every refresh with 400. Each new value grows `windows` with no limit. `window_secs()` quietly treats anything `humantime_secs` cannot parse as 1 h, including `1h30m`, which the server does accept, so the per-second tiles show wrong rates. `logs -f` quietly uses 60 s when `--since` does not parse.
    *Fix:* parse the window once into a `Duration`. Use `humantime::parse_duration`; the workspace already depends on it in `odl-server`. Make `set_since` return `Result`/`bool`, and give clap a typed `value_parser` for `--since`.

14. **medium** — `crates/odl-cli/src/lib.rs:297-353`
    `logs --follow --to X` silently ignores `--to`, because `LogTail::poll` sets `q.to = None`.
    *Fix:* add `#[arg(long, conflicts_with = "follow")]`, or `bail!` when both are given.

15. **medium** — `crates/odl-cli/src/tui/mod.rs:236-252`
    `invalidate` lists every `Kind` except `Stats` by hand. If someone adds a `Kind` and forgets this list, results for the old window are applied after a window change.
    *Fix:* add `Loader::cancel_all_except(&[Kind::Stats])`, or a `Kind::ALL` constant to loop over, so the compiler or the loader keeps the list complete.

16. **medium** — module layout: `crates/odl-cli/src/tui/mod.rs` (732 lines), `crates/odl-cli/src/lib.rs:209-398`, `crates/odl-cli/src/fmt.rs:274`
    - `tui/mod.rs` mixes component state, key routing, the header, tabs, footer and help overlay, duration parsing and the standalone terminal runner.
    - The views add `impl Embedded` blocks and change each other's `pub` state directly: overview writes the traces filter, logs calls `open_trace`.
    - `run` repeats `if out == Output::Json { json(&v) } else { print_x(&v); Ok(()) }` nine times.
    - CLI formatting (`fmt::trace_lines`) depends on `crate::tui::waterfall`.

    *Fix:* split out `tui/chrome.rs` (header, tabs, footer, help), `tui/standalone.rs` and a `duration` helper. Add `fn emit<T: Serialize>(out: Output, v: &T, table: impl FnOnce(&T)) -> Result<()>`. Move `waterfall` to `crate::waterfall` because both the CLI and the TUI use it. Give the views small methods such as `traces.show_for(filter)` instead of writing fields from outside.

17. **medium** — `crates/odl-cli/src/tui/views/logs.rs:242-246`, `crates/odl-cli/src/tui/mod.rs:334-367`
    A successful tail poll never clears `logs.error`, so after recovery an empty list still shows "⚠ <old error>". During an outage, every failed retry (tail every 2 s, trace detail every 3 s) shows a new error flash, so the footer keeps flashing.
    *Fix:* clear `logs.error` in `logs_tail_loaded`. Flash only when a kind goes from OK to error, for example by keeping the last error per `Kind` and comparing.

18. **low (bug)** — `crates/odl-cli/src/tui/views/overview.rs:315-330`
    The operations drill-down shows "loading operations…" forever when the service has no operations or the request failed. The error goes to `overview.error`, which this panel never shows.
    *Fix:* track `loaded`/`error` for operations and show "no operations" or the error.

19. **low** — `crates/odl-cli/src/tui/views/traces.rs:77-86`
    Opening a trace id that does not exist (404) retries every 3 s for as long as the view is open, with an error flash each time.
    *Fix:* back off (3 s, 6 s, 12 s, … up to about 60 s), or after a few attempts treat 404 as "not found yet" and stop flashing.

20. **low** — `crates/odl-cli/src/tui/views/logs.rs:51-73`, `crates/odl-cli/src/tui/loader.rs:49`
    The gap marker is a fake `LogRecord` with `event_name = "\0odl.gap"`. This is a marker stored inside the domain type, and every user has to remember `is_gap`. `Data::Tail((Vec<LogRecord>, bool))` puts a tuple inside a tuple variant.
    *Fix:* use `enum LogRow { Record(LogRecord), Gap(DateTime<Utc>) }` for `logs.records`, and `Data::Tail { records: Vec<LogRecord>, truncated: bool }`.

21. **low** — `crates/odl-cli/src/tui/views/logs.rs:43, 206-216`
    `Arc<tokio::sync::Mutex<LogTail>>` is more than needed: the `is_pending` check plus abort-on-supersede already keep it to one poll at a time. **Uncertain:** this is simplicity only, not a bug. Aborting while the lock is held is safe because `poll` changes state only after its `await`.
    *Fix (optional):* move the `LogTail` into the task and send it back with the result (`Data::Tail { tail, records, truncated }`).

22. **low** — dead code:
    - `loader::STARTED` is written but never read anywhere in the repo, and it is global mutable state (`loader.rs:69-70, 97`).
    - `refresh_logs(force)` is only ever called with `false` (`mod.rs:292`), so its `force` branches never run (`logs.rs:178-201`).
    - The `Cancel` branch in `prompt_key` has an empty `if` (`mod.rs:466-468`).
    - The `"1"` branch of `unit_fmt` (`metrics.rs:94-96`) cannot run, because `chart_fmt` handles `"1"` before calling it.
    - `f.errors_only = false` after `..Default::default()` does nothing (`overview.rs:134`).
    - The `Input` Ctrl-C branch (see 6).

    *Fix:* delete them.

23. **low** — needless clones and allocations:
    - `Embedded::since()` returns a cloned `String` and runs several times per frame (`mod.rs:203, 209, 533`). Return `&str`.
    - `run` clones the `Client` (`lib.rs:210`); borrow `&ctx.client`.
    - `main.rs:27` clones the token; move `cli.token`.
    - `apply_error` clones `e` in every branch (`mod.rs:336-362`); clone once at the end.
    - `Tile::render` clones `value`/`sub` (`widgets.rs:104, 109`); take `self` by value or `impl Widget`.
    - The waterfall sort clones `span_id` for every comparison (`waterfall.rs:36`); use `sort_by(|a, b| (sa.start_time, &sa.span_id).cmp(&(sb.start_time, &sb.span_id)))`.
    - `metrics.visible()` builds a `Vec` and lowercases every name, several times per key and frame (`metrics.rs:59-62`). Cache the lowercased search, or filter with an iterator.

24. **low** — `crates/odl-client/src/lib.rs:110-113`
    `Client::trace` quietly drops every non-hex character. `"xyz"` becomes `GET /api/v1/traces/` with an empty id, which gives a confusing 404 instead of the server's clear "trace id must be 32 hex characters".
    *Fix:* strip only `-` (UUID form), or check for 32 hex characters on the client and return a typed error.

25. **low** — `crates/odl-cli/src/tui/mod.rs:578-580`
    The cursor x position adds `usize` widths cast to `u16` without saturating. A paste of more than 65k characters (no bracketed paste) overflows and panics in debug builds. When the input is wider than the footer, the text does not scroll, so the cursor is clamped onto the wrong character.
    *Fix:* use `saturating_add`, and scroll the visible part of the input so the cursor stays in view.

26. **low** — `crates/odl-cli/src/fmt.rs:54-60` compared with `fmt.rs:14-21`
    `paint_err` treats an empty `NO_COLOR` as "no colour" and ignores `TERM=dumb`; `color()` does the opposite on both. *Fix:* share one `colors_enabled(is_tty: bool)` helper.

27. **low** — `crates/odl-cli/src/tui/mod.rs:17, 46-56`, compared with `crates/sdl-cli/src/lib.rs:17`
    `sdl-cli` re-exports `Embedded` and `Outcome` at the crate root; `odl-cli` only offers them at `odl_cli::tui::…`. ADR 0002 describes the two libraries as having the same shape.
    *Fix:* add `pub use tui::{Embedded, Outcome};`. Do **not** add `#[non_exhaustive]` to `Outcome`: the host is in the same workspace, and an exhaustive match makes the compiler flag every new jump target. Add a comment saying this is deliberate.

28. **low** — `crates/odl-cli/src/tui/mod.rs:213-224, 700`
    `open_trace` reports an invalid id only through a flash message, so the host cannot react. `run_standalone(client, since: String)` takes ownership of a string it only borrows.
    *Fix:* have `open_trace` return `bool` (or `Result`), and take `since: &str`.

29. **low (tests)** — tests are missing for important logic:
    - `Loader`: dropping stale generations, `cancel`, and `load` while a request is pending, including the stale-result case in 2.
    - `LogTail`: the `truncated` flag (1), pruning of `seen` and the `MAX_SEEN` cut.
    - `humantime_secs`, `severity`, `Win::parts`.
    - `nav` on an empty list.
    - JSON versus table output of `run`.

    The HTTP-based tail tests in `tui/regress.rs` cover dedupe and the window only.
    *Fix:* move the dedupe and truncation part of `poll` into a pure `fn accept(&mut self, records: Vec<LogRecord>, limit: u32) -> Vec<LogRecord>` and unit-test it without HTTP. Add `Loader` tests on a current-thread runtime.

30. **low** — `crates/odl-cli/src/tui/views/logs.rs:191-196`
    The "Volume" histogram ignores the `trace=`, `level=` and attribute filters, because `LogSeriesQuery` has no such fields (`odl-api`). Next to a filtered list it looks as if it is filtered too.
    *Fix:* say so in the panel title (for example "Volume · service/text"), or add the fields to the API, which is outside this scope.

31. **low** — `crates/odl-cli/src/tui/views/logs.rs:229`, `crates/odl-cli/src/lib.rs:324-327`
    The first tail poll uses the *client* clock: `Utc::now() - 5s` when the initial page is empty, or `now - since` in the CLI. The `LogTail` doc (`odl-client/src/lib.rs:136-138`) says clock skew does not matter, which is only true from the second poll on.
    *Fix:* make the first poll relative to the server too (`since = lookback` when there is no start record), or narrow the doc.

32. **low** — `crates/odl-cli/src/tui/widgets.rs:262-270`
    `StackedBars` colours each cell by the *lowest* segment that reaches into it (`position(|b| *b > lo)`), but the comment says "segment covering the top of this cell". A thin segment at the top of a cell therefore takes the colour of the one below. Purely visual.
    *Fix:* use the last segment whose lower bound is below `hi`, or correct the comment.

---

### Checked and fine (no change)
- `run` returning `anyhow::Result` is reasonable here. Its only consumers print the error and exit, and `odl-client` already has a typed `Error` at the real library boundary.
- `LogTail::poll` is cancel-safe: all state changes happen after the only `await`. That matters because the CLI `select!`s it against Ctrl-C and the TUI aborts superseded tasks.
- The TUI does not block: loads run on the runtime, results arrive through a non-blocking `try_recv`, and the standalone loop uses `EventStream`.
- No panics found on resize or tiny areas: `draw` guards below 20×6, layouts saturate, and the tests render at 10×3 and 25×8. Wide-character truncation (`widgets::trunc`, waterfall) uses `unicode-width`.
- Waterfall layout handles orphans and cycles and has tests.
