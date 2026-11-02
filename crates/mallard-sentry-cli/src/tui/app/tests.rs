use super::responses::cleaned;
use super::*;
use crate::tui::fixtures::*;

fn key(c: char) -> KeyEvent {
    KeyEvent::from(KeyCode::Char(c))
}

/// The requests of a key press.
fn press(app: &mut App, k: KeyEvent) -> Vec<Req> {
    app.on_key(k).0
}

#[test]
fn stale_issue_lists_are_dropped() {
    let mut app = loaded_app();
    let old = app.issues_key().unwrap();
    press(&mut app, key('s'));
    app.on_response(Resp::Issues(
        old,
        Ok(IssueList { issues: vec![], window_start: now(), window_end: now(), next_cursor: None }),
    ));
    assert_eq!(app.issues.len(), 3, "stale response must not replace the list");
}

#[test]
fn filters_and_search_produce_requests() {
    let mut app = loaded_app();
    let r = press(&mut app, KeyEvent::from(KeyCode::Tab));
    assert!(matches!(&r[1], Req::Issues(k) if k.status == StatusFilter::Resolved));
    press(&mut app, key('/'));
    for c in "boom".chars() {
        assert!(press(&mut app, key(c)).is_empty());
    }
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.query, "boom");
    assert!(matches!(&r[1], Req::Issues(k) if k.query == "boom" && k.query().query.as_deref() == Some("boom")));
    press(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.status, StatusFilter::Unresolved);
    press(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.status, StatusFilter::All, "wraps around");
    press(&mut app, key('3'));
    assert_eq!(app.status, StatusFilter::Ignored);
}

#[test]
fn enter_opens_detail_and_loads_latest_event() {
    let mut app = loaded_app();
    press(&mut app, key('j'));
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.view(), View::Detail);
    assert!(matches!(&r[0], Req::Issue { id, .. } if id == "b2"));
    let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
    assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
    app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
    let d = app.detail().unwrap();
    assert_eq!(d.exceptions.len(), 2);
    // Cursor starts on the first in-app frame.
    assert!(d.stack_frames()[d.frame].frame.in_app);
    // Older event.
    let r = press(&mut app, key('['));
    assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e2"));
    let r = press(&mut app, key(']'));
    assert!(matches!(&r[0], Req::Event { event_id, .. } if event_id == "e1"));
    assert!(press(&mut app, key(']')).is_empty());
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.view(), View::Issues);
}

#[test]
fn resolve_is_optimistic_and_refreshes() {
    let mut app = loaded_app();
    let r = press(&mut app, key('R'));
    assert!(matches!(&r[0], Req::SetStatus { status: IssueStatus::Resolved, .. }));
    assert_eq!(app.issues[0].status, IssueStatus::Resolved);
    let mut d = issue_detail();
    d.summary.status = IssueStatus::Resolved;
    let r = app.on_response(Resp::SetStatus { id: "a1".into(), status: IssueStatus::Resolved, result: Ok(d) });
    assert!(app.toast.as_ref().unwrap().text.contains("resolved"));
    assert!(r.iter().any(|r| matches!(r, Req::Issues(_))));
}

#[test]
fn unknown_and_reserved_keys_are_ignored() {
    let mut app = loaded_app();
    assert_eq!(app.on_key(KeyEvent::from(KeyCode::F(1))).1, Outcome::Ignored);
    assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
    assert_eq!(app.on_key(key('j')).1, Outcome::Consumed);
    assert_eq!(app.on_key(key('q')).1, Outcome::Quit);
    let mut app = detail_app();
    assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
    app.show_trace("t1");
    assert_eq!(app.on_key(key('z')).1, Outcome::Ignored);
}

#[test]
fn trace_view_opens_event_detail() {
    let mut app = detail_app();
    let Outcome::Jump(t) = app.on_key(key('x')).1 else { panic!("x jumps to the trace") };
    assert_eq!(t, "b7644661850f40e69380d8c9ea253e84");
    let r = app.show_trace(&t);
    assert_eq!(r, vec![Req::Trace { trace_id: t.clone() }]);
    assert_eq!(app.view(), View::Trace);
    app.on_response(Resp::Trace { trace_id: t.clone(), result: Ok(event_list()) });
    press(&mut app, key('j'));
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e2")));
    assert_eq!(app.view(), View::Detail);
    // Esc walks back: trace, the detail it was opened from, the list.
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.view(), View::Trace);
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.detail().map(|d| d.id.as_str()), Some("b2"));
    assert_eq!(app.detail().unwrap().event_id.as_deref(), Some("e1"));
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.view(), View::Issues);
}

/// `x` on a span with an error (telemetry tab): the trace's events, then
/// that event's issue; Esc returns to the trace's events.
#[test]
fn trace_event_opens_that_event() {
    let mut app = loaded_app();
    let t = "b7644661850f40e69380d8c9ea253e84";
    app.show_trace_event(t, "e2");
    assert_eq!(app.view(), View::Trace);
    let r = app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
    assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e2")), "{r:?}");
    assert_eq!(app.view(), View::Detail);
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.view(), View::Trace);
    assert_eq!(app.trace().unwrap().state.selected(), Some(1), "the event stays selected");
    // A refresh of the list does not open it again.
    let r = app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
    assert!(r.is_empty() && app.view() == View::Trace, "{r:?}");

    // An unknown event: the list stays in front.
    app.show_trace_event(t, "nope");
    let r = app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
    assert!(r.is_empty() && app.view() == View::Trace, "{r:?}");
}

#[test]
fn loads_reach_screens_behind_the_front_one() {
    let mut app = loaded_app();
    app.issues_state.select(Some(1));
    press(&mut app, KeyEvent::from(KeyCode::Enter));
    let t = "b7644661850f40e69380d8c9ea253e84";
    app.show_trace(t);
    // The detail's loads arrive while the trace view is in front.
    app.on_response(Resp::Issue { id: "b2".into(), result: Ok(issue_detail()) });
    app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
    app.on_response(Resp::Event { event_id: "e1".into(), result: Ok(event_detail()) });
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    let d = app.detail().expect("back on the detail");
    assert!(d.detail.is_some() && d.events_loaded && d.event.is_some());
}

#[test]
fn the_back_stack_is_bounded() {
    let mut app = detail_app();
    for i in 0..3 * MAX_BACK {
        app.show_trace(&format!("t{i}"));
        app.on_response(Resp::Trace { trace_id: format!("t{i}"), result: Ok(event_list()) });
        press(&mut app, KeyEvent::from(KeyCode::Enter));
    }
    assert_eq!(app.back.len(), MAX_BACK);
}

#[test]
fn ctrl_and_alt_keys_do_not_run_actions() {
    let mut app = loaded_app();
    let r = app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(r, (vec![], Outcome::Ignored));
    assert_eq!(app.issues[0].status, IssueStatus::Unresolved);
    assert_eq!(app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)).1, Outcome::Ignored);
    assert_eq!(app.sort, SortBy::LastSeen);
    press(&mut app, key('/'));
    press(&mut app, key('x'));
    press(&mut app, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.input.as_deref(), Some(""));
    // AltGr (ctrl+alt on Windows) types in the search box, and only there.
    press(&mut app, KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
    assert_eq!(app.input.as_deref(), Some("@"));
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    let r = app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL | KeyModifiers::ALT));
    assert_eq!(r, (vec![], Outcome::Ignored));
    assert_eq!(app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)).1, Outcome::Quit);
}

#[test]
fn project_id_is_replaced_by_slug_and_list_reloaded() {
    let mut app = App::new("http://x", Some("1".into()));
    let r = app.start();
    let old = match &r[3] {
        Req::Issues(k) => k.clone(),
        other => panic!("{other:?}"),
    };
    let r = app.on_response(Resp::Projects(Ok(projects())));
    assert_eq!(app.project.as_deref(), Some("web"));
    assert!(r.iter().any(|r| matches!(r, Req::Issues(k) if k.project == "web")), "{r:?}");
    // The response to the old request is stale and dropped.
    app.on_response(Resp::Issues(
        old,
        Ok(IssueList { issues: issues(), window_start: now(), window_end: now(), next_cursor: None }),
    ));
    assert!(!app.issues_loaded);
    // With the slug already given, no extra reload.
    let mut app = App::new("http://x", Some("web".into()));
    app.start();
    assert!(app.on_response(Resp::Projects(Ok(projects()))).is_empty());
}

#[test]
fn stepping_past_the_last_event_appends_the_older_page_and_shows_it() {
    let mut app = detail_app();
    app.detail_mut().unwrap().next_cursor = Some("c1".into());
    press(&mut app, key('['));
    let r = press(&mut app, key('['));
    assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "c1"), "{r:?}");
    let page = EventList { events: vec![event_summary("e3", 300)], next_cursor: None };
    let r = app.on_response(Resp::Events { issue: "b2".into(), before: Some("c1".into()), result: Ok(page) });
    assert!(matches!(&r[..], [Req::Event { event_id, .. }] if event_id == "e3"), "{r:?}");
    let d = app.detail().unwrap();
    assert_eq!(d.events.iter().map(|e| e.event_id.as_str()).collect::<Vec<_>>(), ["e1", "e2", "e3"]);
    assert_eq!((d.event_idx, d.next_cursor.as_deref(), d.want_older), (2, None, false));
}

#[test]
fn older_page_is_requested_once() {
    let mut app = detail_app();
    let d = app.detail_mut().unwrap();
    d.next_cursor = Some("cursor-1".into());
    d.event_idx = 1;
    let r = press(&mut app, key('['));
    assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "cursor-1"));
    assert!(press(&mut app, key('[')).is_empty(), "second press must not load the same page again");
}

#[test]
fn view_errors_are_kept_per_view() {
    let mut app = detail_app();
    app.on_response(Resp::Event { event_id: "e1".into(), result: Err(FetchError::api("boom")) });
    assert_eq!(app.detail().unwrap().event_error.as_deref(), Some("boom"));
    app.on_response(Resp::Info(Ok(info())));
    assert_eq!(app.detail().unwrap().event_error.as_deref(), Some("boom"));
    assert!(app.offline.is_none());
}

#[test]
fn a_panicked_load_is_no_longer_pending() {
    let mut app = App::new("http://x", None);
    app.start();
    assert!(app.in_flight(Kind::Info));
    app.on_response(Resp::Panicked { kind: Kind::Info, msg: "boom".into() });
    assert!(!app.in_flight(Kind::Info), "the kind that panicked must be cleared");
    app.on_response(Resp::Projects(Ok(vec![])));
    assert!(!app.loading());
    // The periodic refresh may load info again.
    app.last_info = Instant::now() - INFO_EVERY;
    assert!(app.on_tick(Instant::now()).contains(&Req::Info));
}

#[test]
fn a_stale_response_does_not_end_a_newer_load() {
    let mut app = loaded_app();
    assert!(!app.loading());
    let first = press(&mut app, key('s'));
    press(&mut app, key('s'));
    let Req::Issues(old) = &first[1] else { panic!("{first:?}") };
    let list = IssueList { issues: issues(), window_start: now(), window_end: now(), next_cursor: None };
    app.on_response(Resp::Issues(old.clone(), Ok(list.clone())));
    app.on_response(Resp::Stats { project: "web".into(), window: app.window, result: Ok(stats()) });
    assert!(app.in_flight(Kind::Issues) && app.in_flight(Kind::Stats), "the newer loads are still running");
    assert!(app.loading());
    app.on_response(Resp::Issues(app.issues_key().unwrap(), Ok(list)));
    app.on_response(Resp::Stats { project: "web".into(), window: app.window, result: Ok(stats()) });
    assert!(!app.loading());
}

#[test]
fn r_retries_failed_trace_events_and_event_loads() {
    let mut app = detail_app();
    app.on_response(Resp::Event { event_id: "e1".into(), result: Err(FetchError::api("boom")) });
    let r = press(&mut app, key('r'));
    assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e1")), "{r:?}");
    assert!(!r.iter().any(|r| matches!(r, Req::Events { .. })), "events are loaded: {r:?}");

    let mut app = loaded_app();
    press(&mut app, KeyEvent::from(KeyCode::Enter));
    app.on_response(Resp::Events { issue: "a1".into(), before: None, result: Err(FetchError::api("boom")) });
    let r = press(&mut app, key('r'));
    assert!(r.iter().any(|r| matches!(r, Req::Events { issue, before: None, .. } if issue == "a1")), "{r:?}");

    let mut app = loaded_app();
    let t = "b7644661850f40e69380d8c9ea253e84";
    app.show_trace(t);
    app.on_response(Resp::Trace { trace_id: t.into(), result: Err(FetchError::offline("down")) });
    let r = press(&mut app, key('r'));
    assert!(r.contains(&Req::Trace { trace_id: t.into() }), "{r:?}");
}

#[test]
fn an_older_page_of_an_earlier_detail_is_dropped() {
    let mut app = detail_app();
    app.detail_mut().unwrap().next_cursor = Some("c1".into());
    press(&mut app, key('4'));
    // Moving to the last event loads the next (older) page.
    let r = press(&mut app, key('G'));
    assert!(matches!(&r[..], [Req::Events { before: Some(c), .. }] if c == "c1"), "{r:?}");
    // Esc and open the same issue again while the older page is on its way.
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    press(&mut app, KeyEvent::from(KeyCode::Enter));
    let older = EventList { events: event_list().events[1..].to_vec(), next_cursor: None };
    app.on_response(Resp::Events { issue: "b2".into(), before: Some("c1".into()), result: Ok(older) });
    let d = app.detail().unwrap();
    assert!(!d.events_loaded && d.events.is_empty(), "stale page appended: {:?}", d.events);
    // The first page arrives and is shown from its first event.
    let r = app.on_response(Resp::Events { issue: "b2".into(), before: None, result: Ok(event_list()) });
    assert!(matches!(&r[..], [Req::Event { event_id, .. }] if event_id == "e1"), "{r:?}");
    let d = app.detail().unwrap();
    assert_eq!((d.events.len(), d.event_idx), (2, 0));
}

#[test]
fn show_trace_closes_the_search_box() {
    let mut app = loaded_app();
    press(&mut app, key('/'));
    press(&mut app, key('x'));
    let t = "b7644661850f40e69380d8c9ea253e84";
    app.show_trace(t);
    assert_eq!(app.input, None);
    app.on_response(Resp::Trace { trace_id: t.into(), result: Ok(event_list()) });
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.view(), View::Detail, "Enter opens the event, not a search");
    assert!(!r.iter().any(|r| matches!(r, Req::Issues(_))), "{r:?}");
}

/// `sanitize` fails closed, so every API type the TUI shows must
/// survive its JSON round trip.
#[test]
fn every_api_type_survives_sanitize() {
    cleaned(info()).unwrap();
    cleaned(projects()).unwrap();
    cleaned(IssueList { issues: issues(), window_start: now(), window_end: now(), next_cursor: None }).unwrap();
    cleaned(stats()).unwrap();
    cleaned(issue_detail()).unwrap();
    cleaned(event_list()).unwrap();
    cleaned(event_detail()).unwrap();
}

/// Review 2.O: `r` means refresh on both tabs; writes are capitals; the
/// jump to the other tab is `x`, and `t` is the time window everywhere.
#[test]
fn shared_keys_mean_the_same_as_on_the_telemetry_tab() {
    let mut app = loaded_app();
    let r = press(&mut app, key('r'));
    assert!(!r.iter().any(|r| matches!(r, Req::SetStatus { .. })), "r must not write: {r:?}");
    assert!(r.iter().any(|r| matches!(r, Req::Issues(_))), "r refreshes: {r:?}");
    assert_eq!(app.issues[0].status, IssueStatus::Unresolved);
    for (k, status) in [('I', IssueStatus::Ignored), ('U', IssueStatus::Unresolved)] {
        let r = press(&mut app, key(k));
        assert!(matches!(&r[0], Req::SetStatus { status: s, .. } if *s == status), "{k}: {r:?}");
    }
    let mut app = detail_app();
    assert!(!matches!(app.on_key(key('t')).1, Outcome::Jump(_)), "t is not the jump key");
    assert!(matches!(app.on_key(key('x')).1, Outcome::Jump(_)));
}

fn app_without_a_choice() -> App {
    let mut app = App::new("http://x", None);
    app.start();
    app.on_response(Resp::Projects(Ok(projects())));
    app
}

fn app_scope(slug: &str, project: Option<&str>) -> Scope {
    Scope::App { slug: slug.into(), project: project.map(str::to_string), services: vec![] }
}

#[test]
fn in_a_host_no_choice_means_no_project() {
    let mut app = App::new("http://x", None);
    assert_eq!(app.start(), vec![Req::Info, Req::Projects]);
    let r = app.on_response(Resp::Projects(Ok(projects())));
    assert_eq!((app.project.as_deref(), r.len()), (None, 0), "All apps asks; it does not pick the first");
    assert!(on_tick_lists_nothing(&mut app));
    // -p stays the first choice.
    let mut app = App::new("http://x", Some("1".into()));
    app.start();
    let r = app.on_response(Resp::Projects(Ok(projects())));
    assert_eq!(app.project.as_deref(), Some("web"), "an id becomes its slug");
    assert!(matches!(r.as_slice(), [Req::Stats { .. }, Req::Issues(_)]));
}

fn on_tick_lists_nothing(app: &mut App) -> bool {
    let later = Instant::now() + REFRESH_EVERY * 2;
    !app.on_tick(later).iter().any(|r| matches!(r, Req::Issues(_) | Req::Stats { .. }))
}

#[test]
fn the_host_chooses_the_project() {
    let mut app = app_without_a_choice();
    let r = app.set_scope(&app_scope("web", Some("web")));
    assert!(matches!(r.as_slice(), [Req::Stats { project, .. }, Req::Issues(_)] if project == "web"));
    assert!(app.toast.as_ref().is_some_and(|t| t.text.starts_with("switched to ")));
    app.on_response(Resp::Issues(
        app.issues_key().unwrap(),
        Ok(IssueList { issues: issues(), window_start: now(), window_end: now(), next_cursor: None }),
    ));
    assert!(!app.issues.is_empty());
    assert!(app.set_scope(&app_scope("web", Some("web"))).is_empty(), "the same app changes nothing");
    // An open screen stays when the same app comes again, and goes with another.
    app.show_trace("abc");
    assert!(app.set_scope(&app_scope("web", Some("web"))).is_empty());
    assert_eq!(app.view(), View::Trace);
    let r = app.set_scope(&app_scope("api", Some("api")));
    assert_eq!(app.view(), View::Issues);
    assert!(app.issues.is_empty(), "nothing of the old project stays");
    assert_eq!(r.len(), 2);
    // An app without a project, then All apps: nothing to load.
    assert!(app.set_scope(&app_scope("worker", None)).is_empty());
    assert_eq!(app.project, None);
    assert_eq!(app.no_project.as_deref(), Some("worker"));
    assert!(app.set_scope(&Scope::All).is_empty());
    assert_eq!(app.no_project, None);
}

#[test]
fn window_and_picker_keys_belong_to_the_host() {
    let mut app = loaded_app();
    for c in ['t', 'T', 'p'] {
        assert_eq!(app.on_key(key(c)), (vec![], Outcome::Ignored), "{c}");
    }
    assert_eq!(app.overlay, Overlay::None);
    // Text in the search box stays text.
    let _ = app.on_key(key('/'));
    for c in ['t', 'p'] {
        assert_eq!(app.on_key(key(c)).1, Outcome::Consumed);
    }
    assert_eq!(app.input.as_deref(), Some("tp"));
}

#[test]
fn the_hosts_window_reloads_what_is_shown() {
    let mut app = loaded_app();
    let r = app.set_window(Window::parse("6h").unwrap());
    assert!(matches!(&r[0], Req::Stats { window: Window::SixHours, .. }));
    assert!(matches!(&r[1], Req::Issues(k) if k.query().since.as_deref() == Some("6h")));
    assert!(app.set_window(Window::SixHours).is_empty(), "the same window");
    for since in ["5m", "15m", "1h", "6h", "24h", "7d", "14d", "30d"] {
        assert_eq!(Window::parse(since).map(Window::since), Some(since), "the host's presets");
        assert!(Window::parse(since).unwrap().interval().ends_with(['s', 'm', 'h']));
    }
    assert_eq!(Window::parse("2d"), None);
}

/// 1h is the host's default, and an app with older issues must not look empty.
#[test]
fn an_empty_list_offers_a_day() {
    let mut app = app_without_a_choice();
    app.set_scope(&app_scope("web", Some("web")));
    app.set_window(Window::Hour);
    assert!(!app.offers_day(), "not while it loads");
    assert_eq!(app.on_key(key('d')).1, Outcome::Ignored);
    app.on_response(Resp::Issues(
        app.issues_key().unwrap(),
        Ok(IssueList { issues: vec![], window_start: now(), window_end: now(), next_cursor: None }),
    ));
    assert!(app.offers_day());
    assert_eq!(app.on_key(key('d')).1, Outcome::SetWindow("24h"));
    app.set_window(Window::Day);
    assert!(!app.offers_day(), "a day is what it offers");
    assert_eq!(app.on_key(key('d')).1, Outcome::Ignored);
    // Not with no app.
    app.window = Window::Hour;
    app.project = None;
    assert!(!app.offers_day());
}

#[test]
fn key_value_words_are_tag_filters() {
    let f = |s: &str| split_filter(s);
    assert_eq!(f(""), (None, vec![]));
    assert_eq!(f("connection refused"), (Some("connection refused".into()), vec![]));
    assert_eq!(f("url=/a boom"), (Some("boom".into()), vec!["url:/a".to_string()]));
    assert_eq!(f("url=/a=b user=\"a b\""), (None, vec!["url:/a=b".to_string(), "user:a b".to_string()]));
    assert_eq!(f("\"a=b\""), (Some("a=b".into()), vec![]), "quoted is text, like the web UI");
    assert_eq!(f("\"a=b c\" x"), (Some("a=b c x".into()), vec![]));
    assert_eq!(f("\"a\"=b"), (Some("a=b".into()), vec![]), "the key has a quote");
    assert_eq!(f("\"a=b\" c=d"), (Some("a=b".into()), vec!["c:d".to_string()]), "each word on its own");
    assert_eq!(f("a=\"b=c\""), (None, vec!["a:b=c".to_string()]), "only the value is quoted");
    assert_eq!(f("=x"), (Some("=x".into()), vec![]));
    let key = IssuesKey {
        project: "web".into(),
        status: StatusFilter::All,
        sort: SortBy::LastSeen,
        window: Window::Day,
        query: "url=/a boom".into(),
        limit: ISSUE_PAGE,
    };
    let q = key.query();
    assert_eq!((q.query.as_deref(), q.tag.as_slice()), (Some("boom"), &["url:/a".to_string()][..]));
    let q = IssuesKey { query: "\"url=/a\"".into(), ..key }.query();
    assert_eq!((q.query.as_deref(), q.tag.as_slice()), (Some("url=/a"), &[][..]), "what is sent to the server");
}

const LATER: Duration = Duration::from_millis(300);

/// The search bar: keys and values from the project's tags, Tab accepts.
#[test]
fn the_search_bar_suggests_tags() {
    let mut app = loaded_app();
    press(&mut app, key('/'));
    assert!(app.on_tick(Instant::now()).iter().all(|r| !matches!(r, Req::Suggest { .. })), "debounced");
    let reqs = app.on_tick(Instant::now() + LATER);
    let Some(Req::Suggest { project, window, scope, want }) =
        reqs.into_iter().find(|r| matches!(r, Req::Suggest { .. }))
    else {
        panic!("no fetch")
    };
    assert_eq!((project.as_str(), window, want.key.clone()), ("web", Window::Day, None));
    assert!(!app.pending.contains_key(&Kind::Suggest), "no spinner");
    let items = Some(vec![("url".to_string(), 7), ("user\x1b[2J".to_string(), 1)]);
    app.on_response(Resp::Suggest { scope: scope.clone(), want, items });
    let labels: Vec<_> = app.suggest.items().iter().map(|i| i.label.clone()).collect();
    assert_eq!(labels, ["url=", "user\\u{1b}[2J="], "cleaned");
    press(&mut app, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.input.as_deref(), Some("url="));
    let reqs = app.on_tick(Instant::now() + LATER);
    let Some(Req::Suggest { want, .. }) = reqs.into_iter().find(|r| matches!(r, Req::Suggest { .. })) else {
        panic!("no fetch for the values")
    };
    assert_eq!(want.key.as_deref(), Some("url"));
    app.on_response(Resp::Suggest { scope, want, items: Some(vec![("/a".into(), 5), ("/b c".into(), 2)]) });
    press(&mut app, KeyEvent::from(KeyCode::Down));
    press(&mut app, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.input.as_deref(), Some("url=\"/b c\" "));
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert!(matches!(&r[1], Req::Issues(k) if k.query().tag == ["url:/b c"]));
    assert!(!app.suggest.has_items(), "closed");
}

/// A failed fetch changes nothing but a small note: no offline banner, no toast.
#[test]
fn a_failed_suggestion_stays_quiet() {
    let mut app = loaded_app();
    press(&mut app, key('/'));
    press(&mut app, key('x'));
    let reqs = app.on_tick(Instant::now() + LATER);
    let Some(Req::Suggest { scope, want, .. }) = reqs.into_iter().find(|r| matches!(r, Req::Suggest { .. })) else {
        panic!("no fetch")
    };
    app.on_response(Resp::Suggest { scope, want, items: None });
    assert_eq!(app.input.as_deref(), Some("x"));
    assert_eq!(app.offline, None);
    assert!(app.toast.is_none());
    assert_eq!(app.suggest.note(), Some("suggestions unavailable"));
    // Nor does it say the server is back.
    app.offline = Some("down".into());
    app.on_response(Resp::Suggest {
        scope: app.suggest.scope().to_string(),
        want: Want { key: None, prefix: String::new() },
        items: Some(vec![]),
    });
    assert_eq!(app.offline.as_deref(), Some("down"));
}

fn issue_list(n: usize) -> IssueList {
    let proto = issues()[0].clone();
    let issues = (0..n).map(|i| IssueSummary { id: format!("i{i}"), ..proto.clone() }).collect();
    IssueList { issues, window_start: now(), window_end: now(), next_cursor: None }
}

/// The app on a list with `n` issues, the first row selected.
fn on_list(n: usize) -> App {
    let mut app = loaded_app();
    let key = app.issues_key().unwrap();
    app.on_response(Resp::Issues(key, Ok(issue_list(n))));
    app
}

#[test]
fn the_last_row_of_a_full_list_reads_more() {
    let mut app = on_list(ISSUE_PAGE as usize);
    let (r, _) = app.on_key(KeyEvent::from(KeyCode::Down));
    assert!(r.is_empty(), "not on the last row yet");
    let (r, _) = app.on_key(KeyEvent::from(KeyCode::Char('G')));
    let [Req::Issues(k)] = r.as_slice() else { panic!("{r:?}") };
    assert_eq!(k.query().limit, Some(400));

    // The answer keeps the selection and the older answer no longer counts.
    let old = IssuesKey { limit: ISSUE_PAGE, ..k.clone() };
    app.on_response(Resp::Issues(old, Ok(issue_list(1))));
    assert_eq!(app.issues.len(), ISSUE_PAGE as usize);
    let k = k.clone();
    app.on_response(Resp::Issues(k, Ok(issue_list(400))));
    assert_eq!(app.issues.len(), 400);
    assert_eq!(app.issues_state.selected(), Some(ISSUE_PAGE as usize - 1));
}

#[test]
fn a_grown_list_holds_still_until_r() {
    let mut app = on_list(ISSUE_PAGE as usize);
    let later = Instant::now() + REFRESH_EVERY * 2;
    assert!(app.on_tick(later).iter().any(|r| matches!(r, Req::Issues(_))), "one page stays live");
    app.pending.clear();

    press(&mut app, key('G'));
    app.pending.clear();
    let key_now = app.issues_key().unwrap();
    app.on_response(Resp::Issues(key_now, Ok(issue_list(400))));
    app.pending.clear();
    let r = app.on_tick(later + REFRESH_EVERY * 2);
    assert!(!r.iter().any(|q| matches!(q, Req::Issues(_))), "past it: no auto refresh: {r:?}");
    assert!(r.iter().any(|q| matches!(q, Req::Stats { .. })), "the rest stays live: {r:?}");
    app.pending.clear();

    let r = press(&mut app, key('r'));
    assert!(r.iter().any(|q| matches!(q, Req::Issues(k) if k.limit == 400)), "r reads the whole list: {r:?}");
}

#[test]
fn a_list_that_is_not_full_has_no_more() {
    let mut app = on_list(3);
    let (r, _) = app.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert!(r.is_empty());
    assert_eq!(app.issue_limit, ISSUE_PAGE);
}

#[test]
fn the_cap_of_the_server_is_said() {
    let mut app = on_list(ISSUE_PAGE as usize);
    app.issue_limit = ISSUE_CAP;
    let key = app.issues_key().unwrap();
    app.on_response(Resp::Issues(key, Ok(issue_list(ISSUE_CAP as usize))));
    let (r, _) = app.on_key(KeyEvent::from(KeyCode::Char('G')));
    assert!(r.is_empty());
    assert!(app.toast.as_ref().is_some_and(|t| t.text.contains("first 500")));
}

#[test]
fn another_sort_starts_again_at_one_page() {
    let mut app = on_list(ISSUE_PAGE as usize);
    press(&mut app, key('G'));
    assert_eq!(app.issue_limit, 400);
    let (r, _) = app.on_key(KeyEvent::from(KeyCode::Char('s')));
    assert_eq!(app.issue_limit, ISSUE_PAGE);
    assert!(r.iter().any(|q| matches!(q, Req::Issues(k) if k.limit == ISSUE_PAGE)), "{r:?}");
}

#[test]
fn an_issue_opens_by_id_in_its_project() {
    let mut app = loaded_app();
    let r = app.show_issue("other", "abc", Some("e1"));
    assert_eq!(app.project.as_deref(), Some("other"));
    let Screen::Detail(d) = &app.screen else { panic!("not on the issue") };
    assert_eq!((d.id.as_str(), d.event_id.as_deref(), d.tab), ("abc", Some("e1"), Some(Tab::Stack)));
    assert!(r.iter().any(|q| matches!(q, Req::Issues(k) if k.project == "other")), "the list behind it: {r:?}");
    assert!(r.iter().any(|q| matches!(q, Req::Event { event_id, .. } if event_id == "e1")), "{r:?}");
    press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.view(), View::Issues);
}
