//! README screenshots of `mallard tui`, rendered from the real TUI into SVG.
//!
//! Needs a running server with demo data (see `scripts/screenshots.sh`):
//!
//! ```sh
//! MALLARD_URL=http://127.0.0.1:28080 MALLARD_TOKEN=dev-token \
//!   cargo test -p mallard -- --ignored screenshots
//! ```
//!
//! Writes `docs/screenshots/*.svg` (or `$MALLARD_SCREENSHOT_DIR`).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::{Terminal, backend::TestBackend};

use super::*;

const COLS: u16 = 132;
const ROWS: u16 = 38;
const CELL_W: f32 = 8.4;
const CELL_H: f32 = 18.0;
const FG: &str = "#d8dee9";
const BG: &str = "#14161c";

/// xterm 256-colour palette entry `i` as `#rrggbb`.
fn indexed(i: u8) -> String {
    const BASE: [&str; 16] = [
        "#1d1f21", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#d8dee9", "#5c6370", "#ff7a85",
        "#b5e890", "#ffd68a", "#82c4ff", "#de9bf0", "#7fd7e0", "#ffffff",
    ];
    match i {
        0..=15 => BASE[i as usize].to_string(),
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            format!("#{:02x}{:02x}{:02x}", level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    }
}

fn color(c: Color, default: &str) -> String {
    match c {
        Color::Reset => default.to_string(),
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
        Color::Indexed(i) => indexed(i),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The buffer as an SVG "terminal window". Every cell sits on a fixed grid,
/// so box drawing and tables line up whatever font the viewer has.
fn to_svg(buf: &Buffer, title: &str) -> String {
    let (w, h) = (buf.area.width, buf.area.height);
    let (pad, top) = (14.0, 34.0);
    let (width, height) = (w as f32 * CELL_W + 2.0 * pad, h as f32 * CELL_H + top + pad);
    let mut bg = String::new();
    let mut fg = String::new();
    for y in 0..h {
        for x in 0..w {
            let cell = &buf[(x, y)];
            let reversed = cell.modifier.contains(Modifier::REVERSED);
            let (mut f, mut b) = (color(cell.fg, FG), color(cell.bg, BG));
            if reversed {
                std::mem::swap(&mut f, &mut b);
            }
            let px = pad + x as f32 * CELL_W;
            let py = top + y as f32 * CELL_H;
            if b != BG {
                let _ = write!(
                    bg,
                    r#"<rect x="{px:.1}" y="{py:.1}" width="{:.1}" height="{CELL_H}" fill="{b}"/>"#,
                    CELL_W + 0.4
                );
            }
            let sym = cell.symbol();
            if sym.trim().is_empty() {
                continue;
            }
            let mut attrs = String::new();
            if cell.modifier.contains(Modifier::BOLD) {
                attrs.push_str(r#" font-weight="bold""#);
            }
            if cell.modifier.contains(Modifier::DIM) {
                attrs.push_str(r#" opacity="0.6""#);
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                attrs.push_str(r#" font-style="italic""#);
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                attrs.push_str(r#" text-decoration="underline""#);
            }
            let _ =
                write!(fg, r#"<text x="{px:.1}" y="{:.1}" fill="{f}"{attrs}>{}</text>"#, py + CELL_H * 0.76, esc(sym));
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}">
<rect width="100%" height="100%" rx="10" fill="{BG}"/>
<circle cx="22" cy="17" r="6" fill="#ff5f57"/><circle cx="42" cy="17" r="6" fill="#febc2e"/><circle cx="62" cy="17" r="6" fill="#28c840"/>
<text x="{:.1}" y="22" fill="#8b93a7" font-family="ui-sans-serif, system-ui, sans-serif" font-size="13" text-anchor="middle">{}</text>
<g>{bg}</g>
<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace" font-size="14" xml:space="preserve">{fg}</g>
</svg>
"##,
        width / 2.0,
        esc(title)
    )
}

fn key(c: KeyCode) -> Event {
    Event::Key(KeyEvent::new(c, KeyModifiers::NONE))
}

/// Let background loads finish: tick and redraw for `d`.
async fn settle(host: &mut Host, term: &mut Terminal<TestBackend>, d: Duration) {
    let end = tokio::time::Instant::now() + d;
    while tokio::time::Instant::now() < end {
        host.tick().unwrap();
        term.draw(|f| host.draw(f)).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn save(term: &Terminal<TestBackend>, dir: &std::path::Path, name: &str, title: &str) {
    let path = dir.join(format!("{name}.svg"));
    std::fs::write(&path, to_svg(term.backend().buffer(), title)).unwrap();
    eprintln!("wrote {}", path.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running server with demo data; see scripts/screenshots.sh"]
async fn screenshots() {
    let url = std::env::var("MALLARD_URL").expect("MALLARD_URL");
    let token = std::env::var("MALLARD_TOKEN").ok();
    let dir = std::env::var_os("MALLARD_SCREENSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/screenshots")));
    std::fs::create_dir_all(&dir).unwrap();

    let client = Client::new(&url, token.as_deref()).unwrap();
    let tabs: Vec<Box<dyn Pane>> = vec![
        Box::new(sdl_cli::Embedded::new(client.sentry.clone(), Some("api".into())).with_hints(crate::ERRORS_HINTS)),
        Box::new(telemetry_pane(&client)),
    ];
    let mut host = Host { tabs, active: ERRORS, base_url: url.clone() };
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();
    let wait = Duration::from_millis(2500);

    settle(&mut host, &mut term, wait).await;
    save(&term, &dir, "errors-issues", "mallard tui — errors");

    host.on_event(&key(KeyCode::Enter));
    settle(&mut host, &mut term, wait).await;
    save(&term, &dir, "errors-issue", "mallard tui — issue detail");

    // Same trace on the telemetry tab: the waterfall with the failing span.
    host.on_event(&key(KeyCode::Char('x')));
    settle(&mut host, &mut term, wait).await;
    save(&term, &dir, "telemetry-trace", "mallard tui — trace of that error");

    // Logs of that trace.
    host.on_event(&key(KeyCode::Char('l')));
    settle(&mut host, &mut term, wait).await;
    save(&term, &dir, "telemetry-logs", "mallard tui — logs of that trace");

    // Fresh telemetry tab for the overview.
    host.tabs[TELEMETRY] = Box::new(telemetry_pane(&client));
    host.active = TELEMETRY;
    settle(&mut host, &mut term, wait).await;
    save(&term, &dir, "telemetry-overview", "mallard tui — telemetry");
}
