//! Parse `exception.stacktrace` text into Sentry frames, best effort.
//!
//! OTel SDKs record the language's own text form. Recognised: Python
//! tracebacks, Java (and Kotlin/Scala) stack traces, V8 (Node, Chrome) and
//! Firefox/Safari JS, Go `debug.Stack()`, .NET `Exception.ToString()` and
//! Rust `std::backtrace`. Frames come back oldest first (Sentry order), with
//! `in_app` false for frames of the runtime and of dependencies. Unknown
//! text gives no frames; the caller keeps the raw text.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

type Parser = fn(&str) -> Vec<Value>;

/// Tried in this order (after the one named by the SDK's language): the
/// stricter formats first, so one format's lines are not read as another's.
const PARSERS: &[(&str, Parser)] = &[
    ("rust", rust),
    ("go", go),
    ("python", python),
    ("java", java),
    ("nodejs", javascript),
    ("webjs", javascript),
    ("dotnet", dotnet),
];

/// Frames of `text`, oldest first; empty when no format fits.
/// `language` is `telemetry.sdk.language` (`python`, `java`, `nodejs`, …).
pub fn parse(text: &str, language: Option<&str>) -> Vec<Value> {
    let hinted = PARSERS.iter().filter(|(l, _)| Some(*l) == language);
    hinted.chain(PARSERS).map(|(_, p)| p(text)).find(|f| !f.is_empty()).unwrap_or_default()
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

#[derive(Default)]
struct Frame<'a> {
    function: Option<&'a str>,
    module: Option<&'a str>,
    file: Option<&'a str>,
    line: Option<&'a str>,
    col: Option<&'a str>,
    context: Option<&'a str>,
    in_app: bool,
}

impl Frame<'_> {
    fn json(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        };
        put("function", self.function.map(Value::from));
        put("module", self.module.map(Value::from));
        put("filename", self.file.map(Value::from));
        put("abs_path", self.file.map(Value::from));
        put("lineno", self.line.and_then(|l| l.parse::<u64>().ok()).map(Value::from));
        put("colno", self.col.and_then(|c| c.parse::<u64>().ok()).map(Value::from));
        put("context_line", self.context.map(Value::from));
        m.insert("in_app".into(), json!(self.in_app));
        Value::Object(m)
    }
}

/// Most recent call first → oldest first.
fn oldest_first(frames: Vec<Frame<'_>>) -> Vec<Value> {
    frames.iter().rev().map(Frame::json).collect()
}

/// `Traceback (most recent call last):` blocks; only the last block (the
/// exception that was raised, after any `During handling …` chain).
fn python(text: &str) -> Vec<Value> {
    static FRAME: LazyLock<Regex> = LazyLock::new(|| re(r#"^\s*File "([^"]+)", line (\d+)(?:, in (.+))?$"#));
    let mut frames: Vec<Frame> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        if l.starts_with("Traceback (most recent call last)") {
            frames.clear();
        }
        let Some(c) = FRAME.captures(l) else { continue };
        let file = c.get(1).map(|m| m.as_str());
        let path = file.unwrap_or_default();
        let context = lines.next_if(|n| n.starts_with("    ") && !FRAME.is_match(n)).map(str::trim);
        frames.push(Frame {
            function: c.get(3).map(|m| m.as_str().trim()),
            file,
            line: c.get(2).map(|m| m.as_str()),
            context,
            in_app: !["site-packages", "dist-packages", "/lib/python", "<frozen "].iter().any(|p| path.contains(p)),
            ..Default::default()
        });
    }
    frames.iter().map(Frame::json).collect()
}

/// `\tat pkg.Class.method(File.java:42)`; the thrown exception's block
/// only (before `Caused by:` / `Suppressed:`).
fn java(text: &str) -> Vec<Value> {
    static FRAME: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"^\s*at (?:[\w.$-]+(?:@[^/]*)?/)?([\w$.<>-]+)\.([\w$<>-]+)\(([^():]+(?::(\d+))?|Native Method|Unknown Source)\)\s*$",
        )
    });
    const RUNTIME: &[&str] = &["java.", "javax.", "jdk.", "sun.", "com.sun.", "kotlin.", "kotlinx.", "scala."];
    let mut frames = Vec::new();
    for l in text.lines() {
        let t = l.trim_start();
        if !frames.is_empty() && (t.starts_with("Caused by:") || t.starts_with("Suppressed:")) {
            break;
        }
        let Some(c) = FRAME.captures(l) else { continue };
        let loc = c.get(3).map_or("", |m| m.as_str());
        // A real location is `File.ext[:line]` or one of the two markers.
        if !(loc.contains('.') || loc == "Native Method" || loc == "Unknown Source") {
            continue;
        }
        let module = c.get(1).map(|m| m.as_str());
        frames.push(Frame {
            function: c.get(2).map(|m| m.as_str()),
            module,
            file: Some(loc.split(':').next().unwrap_or(loc)).filter(|f| f.contains('.')),
            line: c.get(4).map(|m| m.as_str()),
            in_app: !RUNTIME.iter().any(|p| module.unwrap_or_default().starts_with(p)),
            ..Default::default()
        });
    }
    oldest_first(frames)
}

/// `   at Ns.Class.Method(args) in /src/File.cs:line 42`; with inner
/// exceptions, the frames after the last `--- End of inner exception`.
fn dotnet(text: &str) -> Vec<Value> {
    static FRAME: LazyLock<Regex> =
        LazyLock::new(|| re(r"^\s*at ([^\s(]+)\.([^\s.(]+)\([^)]*\)(?: in (.+):line (\d+))?\s*$"));
    let start = text.rfind("--- End of inner exception stack trace ---").unwrap_or(0);
    let frames = text[start..]
        .lines()
        .filter_map(|l| FRAME.captures(l))
        .map(|c| {
            let module = c.get(1).map(|m| m.as_str());
            Frame {
                function: c.get(2).map(|m| m.as_str()),
                module,
                file: c.get(3).map(|m| m.as_str()),
                line: c.get(4).map(|m| m.as_str()),
                in_app: !["System.", "Microsoft."].iter().any(|p| module.unwrap_or_default().starts_with(p)),
                ..Default::default()
            }
        })
        .collect();
    oldest_first(frames)
}

/// V8 (`    at fn (file:1:2)`, `    at file:1:2`) and Firefox/Safari
/// (`fn@file:1:2`) lines.
fn javascript(text: &str) -> Vec<Value> {
    static V8: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*at (?:(.+?) \()?(.+?):(\d+):(\d+)\)?\s*$"));
    static GECKO: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*([^@\s]*)@(.+?):(\d+):(\d+)\s*$"));
    let frames = text
        .lines()
        .filter_map(|l| V8.captures(l).or_else(|| GECKO.captures(l)))
        .map(|c| {
            let file = c.get(2).map(|m| m.as_str().trim_start_matches('('));
            let path = file.unwrap_or_default();
            Frame {
                function: c.get(1).map(|m| m.as_str()).filter(|f| !f.is_empty()),
                file,
                line: c.get(3).map(|m| m.as_str()),
                col: c.get(4).map(|m| m.as_str()),
                in_app: !(path.contains("node_modules") || path.starts_with("node:") || path.starts_with("internal/")),
                ..Default::default()
            }
        })
        .collect();
    oldest_first(frames)
}

/// `debug.Stack()`: a function line, then `\t/path/file.go:12 +0x1d`.
fn go(text: &str) -> Vec<Value> {
    static FILE: LazyLock<Regex> = LazyLock::new(|| re(r"^\t(\S.*\.go):(\d+)(?: \+0x[0-9a-f]+)?$"));
    let mut frames = Vec::new();
    let mut prev: Option<&str> = None;
    for l in text.lines() {
        if let (Some(f), Some(c)) = (prev, FILE.captures(l)) {
            // `created by main.main in goroutine 1` / `main.(*T).m(0x1, …)`
            let f = f.strip_prefix("created by ").unwrap_or(f);
            let f = f.split(" in goroutine ").next().unwrap_or(f);
            let f = match f.rfind('(') {
                Some(i) if f.ends_with(')') && i > 0 && !f[..i].ends_with('.') => &f[..i],
                _ => f,
            };
            // `github.com/a/b/pkg.(*T).Method`: package up to the first `.` after the last `/`.
            let slash = f.rfind('/').map_or(0, |i| i + 1);
            let (module, function) = match f[slash..].find('.') {
                Some(d) => (Some(&f[..slash + d]), &f[slash + d + 1..]),
                None => (None, f),
            };
            let file = c.get(1).map(|m| m.as_str());
            let path = file.unwrap_or_default();
            let pkg = module.unwrap_or_default();
            frames.push(Frame {
                function: Some(function),
                module,
                file,
                line: c.get(2).map(|m| m.as_str()),
                in_app: !(pkg == "runtime"
                    || pkg.starts_with("runtime/")
                    || pkg.starts_with("go.opentelemetry.io/")
                    || path.contains("/go/src/")
                    || path.contains("/pkg/mod/")),
                ..Default::default()
            });
            prev = None;
        } else {
            prev = (!l.is_empty() && !l.starts_with(char::is_whitespace) && !l.starts_with("goroutine ")).then_some(l);
        }
    }
    oldest_first(frames)
}

/// `std::backtrace::Backtrace`: `  3: crate::module::function` and an
/// optional `             at ./src/main.rs:10:5`.
fn rust(text: &str) -> Vec<Value> {
    static FRAME: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*\d+: (.+?)(?:::h[0-9a-f]{16})?\s*$"));
    static AT: LazyLock<Regex> = LazyLock::new(|| re(r"^\s+at (.+?):(\d+)(?::(\d+))?\s*$"));
    const RUNTIME: &[&str] =
        &["std::", "core::", "alloc::", "backtrace::", "tokio::", "__rust", "rust_begin_unwind", "<"];
    let mut frames: Vec<Frame> = Vec::new();
    for l in text.lines() {
        if let Some(c) = FRAME.captures(l) {
            let full = c.get(1).map_or("", |m| m.as_str());
            let (module, function) = match full.rfind("::") {
                Some(i) if !full.starts_with('<') => (Some(&full[..i]), &full[i + 2..]),
                _ => (None, full),
            };
            frames.push(Frame {
                function: Some(function),
                module,
                in_app: !RUNTIME.iter().any(|p| full.starts_with(p)),
                ..Default::default()
            });
        } else if let (Some(last), Some(c)) = (frames.last_mut(), AT.captures(l)) {
            let file = c.get(1).map(|m| m.as_str());
            let path = file.unwrap_or_default();
            last.file = file;
            last.line = c.get(2).map(|m| m.as_str());
            last.col = c.get(3).map(|m| m.as_str());
            last.in_app &= !(path.starts_with("/rustc/") || path.contains("/.cargo/registry/"));
        }
    }
    oldest_first(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(frames: &[Value]) -> Vec<String> {
        frames
            .iter()
            .map(|f| {
                format!(
                    "{}{}:{}@{}{}",
                    f["module"].as_str().map(|m| format!("{m}.")).unwrap_or_default(),
                    f["function"].as_str().unwrap_or("?"),
                    f["lineno"],
                    f["filename"].as_str().unwrap_or("?"),
                    if f["in_app"] == true { "" } else { " (lib)" }
                )
            })
            .collect()
    }

    #[test]
    fn python_traceback_keeps_the_raised_exception() {
        let text = r#"Traceback (most recent call last):
  File "/app/shop/db.py", line 12, in connect
    return pool.get()
KeyError: 'primary'

During handling of the above exception, another exception occurred:

Traceback (most recent call last):
  File "/usr/local/lib/python3.12/site-packages/flask/app.py", line 1473, in wsgi_app
    response = self.full_dispatch_request()
  File "/app/shop/views.py", line 41, in checkout
    order = place_order(cart)
  File "/app/shop/orders.py", line 88, in place_order
    raise ValueError(f"cart {cart.id} is empty")
ValueError: cart 7 is empty
"#;
        let f = parse(text, Some("python"));
        assert_eq!(
            summary(&f),
            [
                "wsgi_app:1473@/usr/local/lib/python3.12/site-packages/flask/app.py (lib)",
                "checkout:41@/app/shop/views.py",
                "place_order:88@/app/shop/orders.py",
            ]
        );
        assert_eq!(f[2]["context_line"], "raise ValueError(f\"cart {cart.id} is empty\")");
        assert_eq!(parse(text, None), f, "detected without a hint");
    }

    #[test]
    fn java_stack_trace_without_causes() {
        let text = "java.lang.IllegalStateException: order 42 has no lines
\tat com.example.shop.OrderService.place(OrderService.java:57)
\tat com.example.shop.OrderController.checkout(OrderController.kt:23)
\tat java.base/jdk.internal.reflect.DirectMethodHandleAccessor.invoke(DirectMethodHandleAccessor.java:103)
\tat java.base/java.lang.Thread.run(Thread.java:1583)
Caused by: java.sql.SQLException: timeout
\tat org.postgresql.Driver.connect(Driver.java:1)
\t... 3 more
";
        let f = parse(text, Some("java"));
        assert_eq!(
            summary(&f),
            [
                "java.lang.Thread.run:1583@Thread.java (lib)",
                "jdk.internal.reflect.DirectMethodHandleAccessor.invoke:103@DirectMethodHandleAccessor.java (lib)",
                "com.example.shop.OrderController.checkout:23@OrderController.kt",
                "com.example.shop.OrderService.place:57@OrderService.java",
            ]
        );
        assert_eq!(parse(text, None), f);
    }

    #[test]
    fn node_stack_trace() {
        let text = "TypeError: Cannot read properties of undefined (reading 'id')
    at getUser (/app/src/users.js:14:22)
    at Layer.handle [as handle_request] (/app/node_modules/express/lib/router/layer.js:95:5)
    at async Promise.all (index 0)
    at /app/src/index.js:30:7
    at process.processTicksAndRejections (node:internal/process/task_queues:95:5)";
        let f = parse(text, Some("nodejs"));
        assert_eq!(
            summary(&f),
            [
                "process.processTicksAndRejections:95@node:internal/process/task_queues (lib)",
                "?:30@/app/src/index.js",
                "Layer.handle [as handle_request]:95@/app/node_modules/express/lib/router/layer.js (lib)",
                "getUser:14@/app/src/users.js",
            ]
        );
        assert_eq!(f[3]["colno"], 22);
        assert_eq!(parse(text, None), f);
        let gecko = parse("handleClick@https://app.example/static/main.js:10:5\n@https://app.example/x.js:1:1", None);
        assert_eq!(
            summary(&gecko),
            ["?:1@https://app.example/x.js", "handleClick:10@https://app.example/static/main.js"]
        );
    }

    #[test]
    fn go_debug_stack() {
        let text = "goroutine 7 [running]:
runtime/debug.Stack()
\t/usr/local/go/src/runtime/debug/stack.go:26 +0x5e
go.opentelemetry.io/otel/sdk/trace.recordStackTrace()
\t/go/pkg/mod/go.opentelemetry.io/otel/sdk@v1.28.0/trace/span.go:479 +0x18
github.com/acme/shop/internal/orders.(*Service).Place(0xc000123, {0x0, 0x0})
\t/src/internal/orders/service.go:88 +0x1d2
main.main()
\t/src/main.go:20 +0x25
";
        let f = parse(text, Some("go"));
        assert_eq!(
            summary(&f),
            [
                "main.main:20@/src/main.go",
                "github.com/acme/shop/internal/orders.(*Service).Place:88@/src/internal/orders/service.go",
                "go.opentelemetry.io/otel/sdk/trace.recordStackTrace:479@/go/pkg/mod/go.opentelemetry.io/otel/sdk@v1.28.0/trace/span.go (lib)",
                "runtime/debug.Stack:26@/usr/local/go/src/runtime/debug/stack.go (lib)",
            ]
        );
        assert_eq!(parse(text, None), f);
    }

    #[test]
    fn dotnet_exception_to_string() {
        let text = "System.InvalidOperationException: Cart is empty
 ---> System.ArgumentNullException: Value cannot be null.
   at Shop.Carts.Load(String id) in /src/Shop/Carts.cs:line 12
   --- End of inner exception stack trace ---
   at Shop.Orders.Place(Cart cart) in /src/Shop/Orders.cs:line 40
   at Shop.Api.OrdersController.Post() in /src/Shop.Api/OrdersController.cs:line 18
   at lambda_method(Closure, Object, Object[])
   at Microsoft.AspNetCore.Mvc.Infrastructure.ActionMethodExecutor.Execute(IActionResultTypeMapper mapper)";
        let f = parse(text, Some("dotnet"));
        assert_eq!(
            summary(&f),
            [
                "Microsoft.AspNetCore.Mvc.Infrastructure.ActionMethodExecutor.Execute:null@? (lib)",
                "Shop.Api.OrdersController.Post:18@/src/Shop.Api/OrdersController.cs",
                "Shop.Orders.Place:40@/src/Shop/Orders.cs",
            ]
        );
        assert_eq!(parse(text, None), f);
    }

    #[test]
    fn rust_backtrace() {
        let text = "   0: std::backtrace_rs::backtrace::libunwind::trace
             at /rustc/abc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:116:5
   1: shop::orders::place::h0123456789abcdef
             at ./src/orders.rs:42:9
   2: shop::main
             at ./src/main.rs:7:5
   3: core::ops::function::FnOnce::call_once
             at /rustc/abc/library/core/src/ops/function.rs:250:5";
        let f = parse(text, Some("rust"));
        assert_eq!(
            summary(&f),
            [
                "core::ops::function::FnOnce.call_once:250@/rustc/abc/library/core/src/ops/function.rs (lib)",
                "shop.main:7@./src/main.rs",
                "shop::orders.place:42@./src/orders.rs",
                "std::backtrace_rs::backtrace::libunwind.trace:116@/rustc/abc/library/std/src/../../backtrace/src/backtrace/libunwind.rs (lib)",
            ]
        );
        assert_eq!(parse(text, None), f);
    }

    #[test]
    fn unknown_text_has_no_frames() {
        assert!(parse("something went wrong\nno frames here", None).is_empty());
        assert!(parse("", Some("python")).is_empty());
    }
}
