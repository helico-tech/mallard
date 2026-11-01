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
fn startup_picks_first_project_and_loads_list() {
    let mut app = App::new("http://x", None);
    let r = app.start();
    assert_eq!(r, vec![Req::Info, Req::Projects]);
    let r = app.on_response(Resp::Projects(Ok(projects())));
    assert_eq!(app.project.as_deref(), Some("web"));
    assert!(matches!(r.as_slice(), [Req::Stats { .. }, Req::Issues(_)]));
}

#[test]
fn stale_issue_lists_are_dropped() {
    let mut app = loaded_app();
    let old = app.issues_key().unwrap();
    press(&mut app, key('s'));
    app.on_response(Resp::Issues(old, Ok(IssueList { issues: vec![], window_start: now(), window_end: now() })));
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
    let r = press(&mut app, key('t'));
    assert!(matches!(&r[0], Req::Stats { window: Window::Week, .. }));
    let r = press(&mut app, key('T'));
    assert!(matches!(&r[0], Req::Stats { window: Window::Day, .. }));
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
    let r = press(&mut app, key('r'));
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
    let Outcome::OpenTrace(t) = app.on_key(key('t')).1 else { panic!("t opens the trace") };
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
    app.on_response(Resp::Issues(old, Ok(IssueList { issues: issues(), window_start: now(), window_end: now() })));
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
    let list = IssueList { issues: issues(), window_start: now(), window_end: now() };
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
    let r = press(&mut app, key('R'));
    assert!(r.iter().any(|r| matches!(r, Req::Event { event_id, .. } if event_id == "e1")), "{r:?}");
    assert!(!r.iter().any(|r| matches!(r, Req::Events { .. })), "events are loaded: {r:?}");

    let mut app = loaded_app();
    press(&mut app, KeyEvent::from(KeyCode::Enter));
    app.on_response(Resp::Events { issue: "a1".into(), before: None, result: Err(FetchError::api("boom")) });
    let r = press(&mut app, key('R'));
    assert!(r.iter().any(|r| matches!(r, Req::Events { issue, before: None, .. } if issue == "a1")), "{r:?}");

    let mut app = loaded_app();
    let t = "b7644661850f40e69380d8c9ea253e84";
    app.show_trace(t);
    app.on_response(Resp::Trace { trace_id: t.into(), result: Err(FetchError::offline("down")) });
    let r = press(&mut app, key('R'));
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
    cleaned(IssueList { issues: issues(), window_start: now(), window_end: now() }).unwrap();
    cleaned(stats()).unwrap();
    cleaned(issue_detail()).unwrap();
    cleaned(event_list()).unwrap();
    cleaned(event_detail()).unwrap();
}

#[test]
fn project_switcher() {
    let mut app = loaded_app();
    press(&mut app, key('p'));
    assert_eq!(app.overlay, Overlay::Projects { selected: 0 });
    press(&mut app, key('j'));
    let r = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.project.as_deref(), Some("api"));
    assert!(matches!(&r[1], Req::Issues(k) if k.project == "api"));
}
