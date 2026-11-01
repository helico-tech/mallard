# Review: `crates/sdl-cli` + `crates/sdl-client`

Scope: every file in both crates was read (about 5.6k lines). Baseline: `cargo test -p sdl-cli -p sdl-client` passes (45 tests).
`cargo clippy -W clippy::pedantic` reports mostly noise (missing `# Errors` sections, numeric casts, `must_use`). The pedantic warnings that matter are included below.
Paths are relative to `crates/`. Findings are ordered with the most important first.

Fixed on branch `fix/review-sdl-cli`: 31 fixed (19, 21, 24, 25 with a part deferred or judged not a problem), 1 not a problem (20), 1 deferred (26). Each finding ends with its status.

## Bugs

1. **bug**: `sdl-cli/src/tui/mod.rs:83-99`, `sdl-cli/src/tui/app.rs:952-956` (host: `mallard/src/tui.rs:161`).
   When a background load panics, `guarded` catches it and sets `app.fatal`. Only the standalone `sdl tui` loop reads `fatal` (`mod.rs:224`), and the field is private, so `Embedded` gives the host no way to see it. The panic hook still runs before `catch_unwind` returns. `mallard tui` uses `ratatui::init()`, whose hook has already left raw mode and the alternate screen, so the host keeps drawing on a cooked terminal. Panics in a load should be rare, but when one happens the terminal is left broken.
   *Fix:* report it to the host. Either add `Outcome::Fatal(String)` (marked `#[non_exhaustive]`) and return it from `handle_event`, or make `tick()` return `Option<Outcome>`, or add `Embedded::take_fatal()`. Update the host to quit on it.

   **Status:** fixed in 8614024 (`Embedded::take_fatal()`; mallard's `Host::tick` returns the error, the loop exits and restores the terminal).

2. **bug**: `sdl-cli/src/tui/app.rs:481-487` vs `ui.rs:72, 695, 818, 859, 941, 978`.
   Error states for the trace view, the event list and the shown event all say "R retry". But `refresh_all` only sends Info, Projects, the list requests and `Req::Issue`. It never sends `Req::Trace`, `Req::Events` or `Req::Event`, and the 5 s auto-refresh does not either. After one failed load the user can only recover by leaving the view and opening it again.
   *Fix:* in `refresh_all`, add `Req::Trace` when `view == Trace` and the trace is not loaded. Add `Req::Events { before: None }` when `!d.events_loaded`, and `Req::Event` for `d.event_id` when `d.event.is_none()`. Add a test.

   **Status:** fixed in 9dbda5c.

3. **bug**: `sdl-cli/src/tui/app.rs:139-140, 155, 932`, `mod.rs:125-138`.
   `Resp::Panicked` does not carry the kind of the request that panicked. So `pending.remove("panicked")` leaves the real kind (for example `"info"`) in `pending` forever. In embedded mode the app keeps running: `on_tick` never re-sends that kind (it filters on `pending`), and the spinner never stops.
   *Fix:* `guarded(kind, fut, tx)` sends `Resp::Panicked { kind, msg }`, and `on_response` removes that kind.

   **Status:** fixed in 13c7c36.

4. **bug**: `sdl-cli/src/tui/app.rs:351, 410-415, 932`.
   `pending: HashSet<&'static str>` is keyed only by kind, but several requests of one kind can be in flight at once (filter changes, reopening an issue). The first, stale response removes the marker while the newer request is still running. Results: the spinner stops early, `on_tick` sends duplicate requests, and the guards `pending.contains("events")` in `step_event`/`move_in_tab` (`:867, :891`) become unreliable. The fixtures hide this by calling `app.pending.clear()` (`fixtures.rs:158, 171`).
   *Fix:* replace the strings with a `Copy` `Kind` enum and keep a count or generation per kind (`HashMap<Kind, u64>`). Each `Req`/`Resp` carries its generation, and a response clears `pending` only when it matches.

   **Status:** fixed in 13c7c36. A count of loads in flight per typed `Kind` instead of a generation: every request gets exactly one response (a panic becomes `Panicked { kind }`), so the count is exact; stale data is still dropped by the keys/ids/cursor in the responses.

5. **bug**: `sdl-cli/src/tui/app.rs:1049-1088`.
   `Resp::Events` is matched only by issue id. Example: an older page (`append = true`) is in flight, and the user presses Esc and reopens the same issue. If the old page arrives first, it is appended to the new, empty `Detail`. Its first event is then selected and loaded. When page 1 arrives, `event_idx` stays 0 while an event from page 2 is shown, and the "event 1/N" label and `[`/`]` navigation are wrong.
   *Fix:* put `before: Option<String>` in `Resp::Events`. Append only when `before == d.next_cursor`, and replace only when `before.is_none()`. The generation counter from finding 4 also fixes this.

   **Status:** fixed in 9dbda5c (`Resp::Events` carries the request's cursor).

6. **bug**: `sdl-cli/src/tui/app.rs:545-554` (with `:526`).
   `show_trace` closes the overlay but not the search box (`self.input`). The host calls it from another tab (the otel "errors of this trace" jump). If the errors tab was in the middle of a search, every key then goes to `on_key_search` inside the trace view, and Enter reloads the issue list instead of opening the event.
   *Fix:* set `self.input = None` in `show_trace`. Add a test.

   **Status:** fixed in 9dbda5c.

## High

7. **high**: `sdl-cli/src/cli.rs:31-53, 90, 93, 228, 279, 414, 710`, `tui/ui.rs:344`, `tui/mod.rs:193`.
   Error messages and hints hard-code `SDL_TOKEN`, `SDL_URL`, `SDL_PROJECT`, `sdl projects create`, `sdl events show … -p` and `sdl send-test`. When the commands are mounted as `mallard errors …` (ADR 0002), the hints are wrong: the env vars are `MALLARD_URL`/`MALLARD_TOKEN`/`MALLARD_PROJECT` and the commands are `mallard errors …`.
   *Fix:* add a hint profile (`program: &'static str`, plus env names) that the host passes in through `Ctx`, and the same for `Embedded`. The simpler option is to make the hints generic ("check the token", "create a project with `projects create`"). I prefer the profile: it keeps the hints actionable.

   **Status:** fixed in e5410b8 (`Hints` on `Ctx` and `Embedded`; mallard passes `mallard errors` / `MALLARD_*`). Also `-p/--project` is global on `mallard errors`, so it works after the subcommand.

## Medium

8. **medium**: `sdl-cli/src/fmt.rs:213-217`.
   `sanitize` fails open. If the serialize → `Value` → deserialize round trip fails, it returns the **unsanitized** original (`unwrap_or(v)`). All protection against terminal-escape injection rests on this function. I found no current type that fails the round trip (no `f64`; the `flatten` fields round-trip), but a future field could (for example an `f64` NaN, which serializes to `null`), and it would fail silently.
   *Fix:* fail closed. Return `Result<T, serde_json::Error>` and turn a failure into an error, or at least add `debug_assert!` and a test over every API type.

   **Status:** fixed in 97a8fc7 (`sanitize` returns `Result`; the CLI fails, the TUI shows "cannot show the response"; a test runs every API type through it).

9. **medium**: `sdl-cli/src/cli.rs:74-76`.
   `Ctx::err` turns `sdl_client::Error` into `anyhow!(String)` and drops the typed source. Hosts cannot downcast it (`is_not_found`, mapping to exit codes). It also takes `e` by value without consuming it (pedantic `needless_pass_by_value`).
   *Fix:* `anyhow::Error::new(e).context(describe(&e_ref, url))`. The top-level `Display` is unchanged, so `main` prints the same text and the chain is kept. Make `err` `pub(crate)`.

   **Status:** fixed in e5410b8 (the client error is the source; its server message is cleaned too, because mallard prints the whole chain).

10. **medium**: `sdl-client/src/lib.rs:17-25, 40-45` and `sdl-cli/src/cli.rs:46`.
    `Error::Url(String)` covers both "bad base URL" and "bad path segment". `describe` tells them apart with `m.starts_with("invalid path segment")`, which is a string-typed contract. The enum is also not `#[non_exhaustive]`.
    *Fix:* add `Error::InvalidSegment(String)` and match on the variant. Mark `Error` `#[non_exhaustive]`, and `describe` keeps a `_` arm.

    **Status:** fixed in c31b8d4.

11. **medium**: `sdl-client/src/lib.rs:47-52`.
    `#[derive(Debug)]` on `Client` prints the bearer token. Any `{:?}` of `Client` or of a struct containing it (for example `mallard_client::Client`) leaks the token into logs or panic output.
    *Fix:* write `Debug` by hand with `.field("token", &"<redacted>")`.

    **Status:** fixed in ec44643.

12. **medium**: `sdl-client/src/lib.rs:83-88`.
    `api_error` reads the whole error body and uses it as the message when it is not `ApiError` JSON. A proxy's 502 HTML page then ends up, unbounded, in the "one-line" CLI error and in TUI toasts.
    *Fix:* cap the body (for example the first 200 chars, cut on a char boundary) and fall back to `status.canonical_reason()` when the content type is not JSON.

    **Status:** fixed in ba7a38b. Plain-text bodies (e.g. axum's query rejections) are kept, cut to 200 chars; other non-JSON bodies become the status reason.

13. **medium**: `sdl-client/src/lib.rs:56, 74, 171`, `sdl-cli/src/main.rs:12`.
    The token is a required `&str`, and callers use `""` as "no token" (`default_value = ""`, mallard's `unwrap_or("")`). Every request then sends `Authorization: Bearer ` with an empty value.
    *Fix:* use `Client::new(base_url, token: Option<impl Into<String>>)` and call `bearer_auth` only when it is `Some`.

    **Status:** fixed in ec44643, without the signature change: an empty token now means no `Authorization` header. `Client::new(&str, Option<..>)` would break `sdl-loadgen` and `mallard-client`, which are out of this scope.

14. **medium (uncertain)**: `sdl-client/src/lib.rs:60, 170-173`.
    The 30 s client-wide timeout also applies to `maintenance()` (flush + compact + expire). On a large lake that can take longer. The CLI then reports "request timed out" while the server keeps working.
    *Fix:* override it for this request with `RequestBuilder::timeout(Duration::from_secs(600))`. I did not measure real maintenance times.

    **Status:** fixed in ba7a38b (10 minutes for `maintenance()`).

15. **medium**: `sdl-cli/src/lib.rs:9-17, 21-174`, `tui/mod.rs:30-40`, `cli.rs:65-71`.
    The public surface is larger than the documented API (`Command`/`run`/`Ctx`/`Embedded`/`Outcome`). `pub mod event_view` (no crate outside uses it) and `pub mod tui` (exposes `tui::run(&Ctx)`) are also public. `Outcome`, `Command` and the sub-enums are not `#[non_exhaustive]`, and `Outcome` is not `#[must_use]`. `Ctx` is built from a struct literal with public fields, so adding a field (for example the hint profile from finding 7) breaks `mallard`. Clippy `unreachable_pub` also reports 120 `pub` items inside private modules.
    *Fix:* make `event_view` and `tui` `pub(crate)` and re-export only `Embedded`/`Outcome`. Add `#[non_exhaustive]` + `#[must_use]` to `Outcome`. Add `Ctx::new(client)` with setters, or mark `Ctx` `#[non_exhaustive]`. Use `pub(crate)` inside the private modules.

    **Status:** fixed in e5410b8 (`Ctx::new` + `with_*`), e981c6f (`Outcome` `#[non_exhaustive]` + `#[must_use]`) and e49bbfc (`event_view`/`tui` private, `Command` and sub-enums `#[non_exhaustive]`, `pub(crate)` in private modules, `#![warn(unreachable_pub)]`).

16. **medium**: `sdl-cli/src/tui/app.rs:355-360, 499-500, 79-81 (mod.rs), 557-560, 772-775`, `ui.rs:440, 969`.
    The state machine allows states it should not:
    - `on_key` reports results through flags (`quit`, `ignored`, `open_trace`) that `Embedded` must read and reset by hand (`quit` is reset in `mod.rs:80`, while `ignored` stays set until the next key).
    - `View::Detail` can exist with `detail: None`, and `View::Trace` with `trace: None`. Both are handled defensively: input falls back to Issues, and drawing renders an empty body.

    *Fix:* return `(Vec<Req>, Effect)` from `on_key`, where `Effect` is `None`, `Ignored`, `Quit` or `OpenTrace(String)`. Model navigation as data-carrying variants (`enum Screen { Issues, Detail(Detail), Trace(TraceView) }` plus a small back stack), so the invalid combinations cannot be built.

    **Status:** fixed in e981c6f (`Screen::{Issues, Detail, Trace}` + bounded back stack; `on_key` returns `(Vec<Req>, Outcome)`).

17. **medium**: `sdl-cli/src/tui/app.rs:13-17, 23-43, 326-329, 1135-1139`, `ui.rs:349`, `cli.rs:79-95, 283-309`.
    Filters are `usize` indexes into const string arrays (`STATUSES`, `SORTS`, `WINDOWS`), with a magic `app.status == 3`. The same status and sort lists exist a second time as the CLI's `StatusFilter`/`SortBy` plus the hand-written mapping in `issue_query`. The `SetStatus` verb `match` copies `IssueStatus::as_str()` exactly.
    *Fix:* use one `StatusFilter`/`SortBy`/`Window` enum, each with `as_str()` and an `ALL` const, in both the CLI and the TUI. Cycle with a small `next()`/`prev()`. Replace the verb `match` with `status.as_str()`.

    **Status:** fixed in c8aad05.

18. **medium**: module layout, `sdl-cli/src/tui/app.rs` (1337 lines; `on_response` is 218 lines, `:930`), `tui/ui.rs` (1343 lines; `render_overview` 123 lines, `render_stack` 125 lines), `cli.rs` (727 lines; error mapping, `Ctx`, and the printing for every command in one file).
    *Fix:* split `ui.rs` into `ui/{issues,detail,trace,overlays}.rs`. Split `app.rs` into state types, `keys.rs` and `responses.rs`, with one handler per `Resp` variant. Move `describe` and `Ctx` out of the printers in `cli.rs` (for example `cli/{mod,render}.rs`).

    **Status:** fixed in ff63590 (`ui/`, `app/{mod,keys,responses,tests}`, one handler per `Resp`) and f8351c3 (`cli/{mod,commands}`).

## Low

19. **low**: `sdl-cli/src/tui/mod.rs:116-120`.
    Spawned loads are never cancelled. Superseded loads (fast `[`/`]`, filter changes) and every in-flight task after `Embedded` is dropped keep running for up to 30 s. The host also ticks hidden tabs, so a hidden errors tab keeps polling every 2 s and 5 s, and there is no pause API.
    *Fix:* keep an `AbortHandle` per kind (or a `JoinSet`). Abort the superseded request when a new one of the same kind starts, and abort all of them in `Drop`. Optionally add `set_visible(bool)`.

    **Status:** partly fixed in 0d47843: dropping `Embedded` aborts its loads. Deferred: aborting superseded loads (the in-flight counts of #4 rely on every request answering, so it needs cancel-aware bookkeeping) and pausing hidden tabs (a new host API, `set_visible`).

20. **low**: `sdl-cli/src/tui/mod.rs:58-62`.
    `Embedded::new` panics outside a tokio runtime (`Handle::current()`).
    *Fix:* add `Embedded::with_handle(client, project, Handle)` or a `try_new` that returns `Result`, so the panic is not part of the API.

    **Status:** not a problem: same contract as `tokio::spawn` / `Handle::current`, documented on `new` and in the README; the only host calls it inside the runtime.

21. **low**: needless clones and repeated work in the draw path.
    - `ui.rs:967` clones `app.projects` on every frame. Destructure instead: `let App { projects, trace, now, .. } = app;`.
    - `ui.rs:445, 559` clone the whole `IssueSummary` (including the trend `Vec`) on every frame. Borrow it instead.
    - `ui.rs:35` and `app.rs:518` call `overlay.clone()`. Derive `Copy` on `Overlay`.
    - `ui.rs:610-618, 699` re-parse `contexts`/`request`/`message` from the payload on every frame. Cache them in `Detail::set_event` like `exceptions`/`crumbs`.
    - `Detail::stack_frames()` builds a new `Vec` on every key press and every draw (`app.rs:801, 845`).
    - `app.rs:777` clones `d.id` on every key.

    **Status:** fixed in e981c6f (projects no longer cloned, `Overlay` is `Copy`, no `d.id` clone per key) and 1d958ae (summary borrowed; contexts/request/message parsed once per event). `stack_frames()` building a `Vec`: not a problem (a few dozen references per key press or frame).

22. **low**: `sdl-cli/src/tui/ui.rs:668, 810` (also `:359, :1081`).
    `usize as u16` silently truncates scroll offsets and lengths. A very long stack (many frames plus context) or tag list would scroll to the wrong place.
    *Fix:* use `u16::try_from(x).unwrap_or(u16::MAX)`, or `skip`/`take` the lines yourself as `render_json` already does.

    **Status:** fixed in 1d958ae.

23. **low**: Unicode width.
    `fmt::truncate`/`truncate_left` and the `{:<N}` padding count `chars()`, not display columns (`cli.rs:396-398, 602-604`; `ui.rs:652-654, 753, 765-767`). CJK text and emoji misalign the tag bars, source context and tables. Nothing panics.
    *Fix:* measure with `unicode-width`, which ratatui already depends on, or with `Span::width()`.

    **Status:** fixed in 1d958ae (`unicode-width`; `fmt::pad` replaces `{:<N}`). Found on the way: the CLI padded strings that already held color codes, so colored columns were not aligned; it now pads first.

24. **low**: duplicated logic.
    - The bidi-control set appears in both `fmt.rs:182` and `cli.rs:123`. Share an `is_bidi_control(char)` helper.
    - The handled/unhandled label is in `cli.rs:551-555` and `ui.rs:721-725`. Move it to `Exception::mechanism_label()`.
    - The project-switch reset is in `app.rs:591-595` and `app.rs:630-635`. Extract `switch_project()`.
    - The older-page request is in `app.rs:865-872` and `app.rs:891-899`.
    - `Req::kind`/`Resp::kind` keep two parallel string tables.
    - The `screen()` test helper exists twice (`mod.rs:248`, `ui.rs:1129`).
    - The TTY/`NO_COLOR` check is in `main.rs:35` and `paint.rs:10`.

    **Status:** fixed in 1d958ae (`is_bidi_control`, `Exception::mechanism_label`), e981c6f (`switch_project`, `Detail::older_page`) and 0bf3524 (one `screen()` test helper). `Req::kind`/`Resp::kind` now return a typed `Kind` (13c7c36); both stay, because a panicked response must still name its kind. TTY/`NO_COLOR` check: deferred: cross-side (it goes with #26).

25. **low**: dead or redundant API.
    - `Client::resolve`/`ignore`/`unresolve` (`sdl-client/src/lib.rs:129-139`) are not used anywhere in the workspace, because `set_issue_status` covers them.
    - `cli::print_event` is `pub` but cannot be reached from outside.
    - `Frame::has_context` (`event_view.rs:63`) and `Detail::current_event_id` (`app.rs:305`) are trivial wrappers.
    - `spinner_static` (`ui.rs:539`) duplicates `spinner(app)`.
    - The `events_by_trace` and `events_by_trace_query` pair is awkward. One `events_for_trace(&TraceQuery)` is enough.
    - `maintenance()` copies the logic of `send`. Add a `send_empty` helper.

    **Status:** fixed in ba7a38b (`resolve`/`ignore`/`unresolve` removed; `maintenance()` shares `checked()` with `send`), 1d958ae (`Frame::has_context`), e981c6f (`current_event_id`, `spinner_static`) and f8351c3 (`print_event` private). `events_by_trace` + `events_by_trace_query`: deferred: `sdl-loadgen` (out of scope) calls `events_by_trace`.

26. **low**: `sdl-cli/src/paint.rs:7-12`, `cli.rs:108`.
    `run` writes a process-global `AtomicBool` on every call. This goes against the rule in architecture.md that library crates have no global side effects, and it would race if a host ran two commands at once.
    *Fix:* compute a `Painter { color: bool }` once in `run` and pass it to the printers.

    **Status:** deferred: cross-side (the shared terminal-output crate will own color detection for both sides).

27. **low**: wrong or stale docs.
    - `tui/mod.rs:4` says "obs-ducklake"; the host is `mallard`.
    - `paint.rs:42` says "reverse video", but the code is bold yellow.
    - `paint.rs:47` says fatal is red, but the code uses magenta.

    **Status:** fixed in 0d47843.

28. **low**: `sdl-cli/src/cli.rs:125`.
    `json_safe` allocates a `String` for every escaped char (`push_str(&format!(..))`). Use `write!(out, "\\u{:04x}", c as u32)` with `std::fmt::Write`. The pedantic lint `format_push_string` also flags `event_view.rs:55, 57`.

    **Status:** fixed in 1d958ae.

29. **low**: `sdl-cli/src/tui/mod.rs:208`.
    `tokio::time::interval` uses the default `MissedTickBehavior::Burst`, so after a stall it fires a burst of ticks (and redraws). Call `ticker.set_missed_tick_behavior(MissedTickBehavior::Skip)`.

    **Status:** fixed in 0d47843.

30. **low**: `sdl-cli/src/fmt.rs:140`.
    In `parse_duration`, `n * mult` can overflow: it panics in debug and wraps in release. Today it only receives the constants in `WINDOWS`, but the function is a general helper. Use `n.checked_mul(mult)`.

    **Status:** fixed in 1d958ae.

31. **low (uncertain, Windows only)**: `sdl-cli/src/tui/app.rs:510-516`.
    On Windows, crossterm reports AltGr as CONTROL|ALT, so `@`, `{`, `\` and similar characters on European keyboards are ignored in the search box. Fix: while `input` is active, accept `Char(c)` when the modifiers are exactly CONTROL|ALT.

    **Status:** fixed in e981c6f (with a test; not tried on a real Windows terminal).

32. **low**: `sdl-cli/src/main.rs:41`.
    `std::process::exit(1)` inside `#[tokio::main]` skips destructors. Return `std::process::ExitCode` from `main` instead.

    **Status:** fixed in 0d47843.

33. **low**: test gaps for logic that matters.
    - `describe()` status and variant mapping: there is only one integration test, for the 500 case.
    - The `Resp::Events` append and `want_older` response path (only the request side is tested).
    - `Resp::Panicked` cleanup of `pending`, "R retry" for trace/event loads, and `show_trace` during a search (findings 2, 3, 6).
    - In `sdl-client`: `api_error` with a JSON vs a raw body, and a non-JSON body from `maintenance`. Neither has a test.
    - The `json_line` highlighter and `parse_ts` (numeric strings, fractional and negative timestamps).
    - The fixtures call `app.pending.clear()`, which hides the pending bugs.

    **Status:** fixed in e5410b8 (`describe`), ba7a38b (client `api_error` and `maintenance` bodies), 13c7c36 / 9dbda5c (findings 2, 3, 6; fixtures no longer clear `pending`) and 0bf3524 (older-page path, `json_line`, `parse_ts`; `parse_ts` used `trunc` and lost the fraction of negative timestamps, now `floor`).
