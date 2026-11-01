//! JavaScript sourcemap symbolication.
//!
//! Two steps, so the caller can fetch maps asynchronously in between:
//!
//! 1. [`collect_refs`] lists the minified files (`abs_path`) the event's
//!    frames point at, with the debug id from `debug_meta.images` if known.
//! 2. The caller resolves each ref to a [`ResolvedSourceMap`] (debug id
//!    first, then release + dist + url) and calls [`apply`], which rewrites
//!    frames to original file/line/column/function and adds source context.
//!
//! Like Sentry, the original frames are kept in `raw_stacktrace`.
//! Frame line/column numbers are 1-based; sourcemap tokens are 0-based.

use serde_json::{Value, json};
use sourcemap::{DecodedMap, SourceMap, SourceView};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

const CONTEXT_LINES: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceMapRef {
    pub abs_path: String,
    pub debug_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct ResolvedSourceMap {
    pub map: Arc<SourceMap>,
    /// The minified file, used to recover original function names.
    pub minified: Option<Arc<SourceView>>,
    /// URL (or bundle path) of the map; relative sources resolve against it.
    pub map_url: String,
    /// "debug-id" or "release", reported in `frame.data.resolved_with`.
    pub resolved_with: &'static str,
}

impl std::fmt::Debug for ResolvedSourceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSourceMap").field("map_url", &self.map_url).finish_non_exhaustive()
    }
}

/// Parse a regular or indexed sourcemap into a flat [`SourceMap`].
pub fn parse_sourcemap(bytes: &[u8]) -> Result<SourceMap, sourcemap::Error> {
    match sourcemap::decode_slice(bytes)? {
        DecodedMap::Regular(sm) => Ok(sm),
        DecodedMap::Index(idx) => idx.flatten(),
        DecodedMap::Hermes(h) => Ok((*h).clone()),
    }
}

fn strip_file_scheme(p: &str) -> &str {
    p.strip_prefix("file://").unwrap_or(p)
}

/// Every stack trace in the event: exceptions, threads, top level.
fn for_each_stacktrace_mut(ev: &mut Value, mut f: impl FnMut(&mut Value, bool)) {
    for key in ["exception", "threads"] {
        if let Some(values) = ev.get_mut(key).and_then(|e| e.get_mut("values")).and_then(Value::as_array_mut) {
            for v in values {
                if v.get("stacktrace").is_some() {
                    f(v, true);
                }
            }
        }
    }
    if ev.get("stacktrace").is_some() {
        f(ev, false);
    }
}

fn frames(holder: &Value) -> impl Iterator<Item = &Value> {
    holder.get("stacktrace").and_then(|s| s.get("frames")).and_then(Value::as_array).into_iter().flatten()
}

fn is_symbolicatable(frame: &Value) -> bool {
    frame.get("abs_path").and_then(Value::as_str).is_some()
        && frame.get("lineno").and_then(Value::as_u64).is_some()
        && frame.get("colno").and_then(Value::as_u64).is_some()
        && frame.get("data").and_then(|d| d.get("symbolicated")).is_none()
}

/// The debug ids announced in `debug_meta.images` keyed by code file.
fn debug_ids(ev: &Value) -> HashMap<String, Uuid> {
    ev.get("debug_meta")
        .and_then(|d| d.get("images"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|img| img.get("type").and_then(Value::as_str) == Some("sourcemap"))
        .filter_map(|img| {
            let file = img.get("code_file")?.as_str()?;
            let id = Uuid::try_parse(img.get("debug_id")?.as_str()?).ok()?;
            Some((strip_file_scheme(file).to_string(), id))
        })
        .collect()
}

/// The distinct minified files (at most `limit`) that the event's frames
/// point at, in frame order.
pub fn collect_refs(ev: &Value, limit: usize) -> Vec<SourceMapRef> {
    let ids = debug_ids(ev);
    let mut seen: HashSet<&str> = HashSet::new();
    let holders = ["exception", "threads"]
        .into_iter()
        .filter_map(|key| ev.get(key).and_then(|e| e.get("values")).and_then(Value::as_array))
        .flatten()
        .chain(std::iter::once(ev));
    holders
        .flat_map(frames)
        .filter(|f| is_symbolicatable(f))
        .filter_map(|f| f["abs_path"].as_str())
        .filter(|abs| seen.insert(abs))
        .take(limit)
        .map(|abs| SourceMapRef { abs_path: abs.to_string(), debug_id: ids.get(strip_file_scheme(abs)).copied() })
        .collect()
}

/// Release-artifact url candidates for a minified file, most specific first:
/// `https://x.test/static/app.js?v=1` -> `~/static/app.js?v=1`, `~/static/app.js`.
pub fn url_candidates(abs_path: &str) -> Vec<String> {
    let mut out = vec![abs_path.to_string()];
    if let Ok(u) = url::Url::parse(abs_path) {
        let path = u.path();
        if let Some(q) = u.query() {
            out.push(format!("~{path}?{q}"));
        }
        out.push(format!("~{path}"));
    } else if abs_path.starts_with('/') {
        out.push(format!("~{abs_path}"));
    }
    out.dedup();
    out
}

/// Resolve a map `source` entry to an absolute path and a display filename.
fn resolve_source(source: &str, map_url: &str) -> (String, String) {
    if let Some(rest) = source.strip_prefix("webpack://") {
        // webpack://<namespace>/./src/x.ts -> src/x.ts
        let path = rest.split_once('/').map_or(rest, |(_, p)| p);
        let path = path.trim_start_matches("./");
        return (source.to_string(), path.to_string());
    }
    let abs = match url::Url::parse(source) {
        Ok(u) => u.to_string(),
        Err(_) => match url::Url::parse(map_url).and_then(|base| base.join(source)) {
            Ok(u) => u.to_string(),
            Err(_) => source.to_string(),
        },
    };
    let filename = match url::Url::parse(&abs) {
        Ok(u) if matches!(u.scheme(), "http" | "https" | "file" | "app") => u.path().to_string(),
        _ => abs.trim_start_matches("./").to_string(),
    };
    (abs, filename)
}

/// Lines around `line` of a map source. The map's `SourceView` keeps a line
/// index (built once per cached map), so this does not rescan the source for
/// every frame.
fn context(sm: &SourceMap, src_id: u32, line: u32) -> Option<(Vec<Value>, String, Vec<Value>)> {
    let sv = sm.get_source_view(src_id)?;
    let current = sv.get_line(line)?;
    let pre = (line.saturating_sub(CONTEXT_LINES)..line).filter_map(|i| sv.get_line(i)).map(|l| json!(l)).collect();
    let post =
        (line + 1..=line.saturating_add(CONTEXT_LINES)).map_while(|i| sv.get_line(i)).map(|l| json!(l)).collect();
    Some((pre, current.to_string(), post))
}

fn rewrite_frame(frame: &mut Value, rm: &ResolvedSourceMap) -> bool {
    let (Some(line), Some(col)) =
        (frame.get("lineno").and_then(Value::as_u64), frame.get("colno").and_then(Value::as_u64))
    else {
        return false;
    };
    // Frame numbers are untrusted u64; out-of-range lines must not wrap.
    let (Some(Ok(line0)), Ok(col0)) = (line.checked_sub(1).map(u32::try_from), u32::try_from(col.saturating_sub(1)))
    else {
        return false;
    };
    let Some(token) = rm.map.lookup_token(line0, col0) else { return false };
    let Some(source) = token.get_source() else { return false };
    let minified_fn = frame.get("function").and_then(Value::as_str).map(str::to_string);

    let original_fn = minified_fn.as_deref().and_then(|f| {
        let short = f.rsplit('.').next().unwrap_or(f);
        let sv = rm.minified.as_ref()?;
        let orig = rm.map.get_original_function_name(line0, col0, short, sv)?;
        Some(if short.len() == f.len() { orig.to_string() } else { format!("{}{}", &f[..f.len() - short.len()], orig) })
    });
    let (abs_path, filename) = resolve_source(source, &rm.map_url);
    let src_line = token.get_src_line();
    let fo = frame.as_object_mut().expect("frame is object");
    fo.insert("lineno".into(), json!(u64::from(src_line) + 1));
    fo.insert("colno".into(), json!(token.get_src_col() + 1));
    let in_app = !(abs_path.contains("/node_modules/") || abs_path.contains("webpack/bootstrap"));
    fo.insert("in_app".into(), json!(in_app));
    fo.insert("abs_path".into(), json!(abs_path));
    fo.insert("filename".into(), json!(filename));
    if let Some(f) = original_fn {
        fo.insert("function".into(), json!(f));
    } else if minified_fn.is_none()
        && let Some(name) = token.get_name()
    {
        fo.insert("function".into(), json!(name));
    }
    fo.remove("pre_context");
    fo.remove("context_line");
    fo.remove("post_context");
    if let Some((pre, cur, post)) = context(&rm.map, token.get_src_id(), src_line) {
        fo.insert("pre_context".into(), json!(pre));
        fo.insert("context_line".into(), json!(cur));
        fo.insert("post_context".into(), json!(post));
    }
    let data = fo.entry("data").or_insert_with(|| json!({}));
    if let Value::Object(d) = data {
        d.insert("sourcemap".into(), json!(rm.map_url));
        d.insert("symbolicated".into(), json!(true));
        d.insert("resolved_with".into(), json!(rm.resolved_with));
    }
    true
}

/// Rewrite frames using the resolved maps (keyed by minified `abs_path`).
/// Returns the number of frames rewritten.
pub fn apply(ev: &mut Value, maps: &HashMap<String, Arc<ResolvedSourceMap>>) -> usize {
    if maps.is_empty() {
        return 0;
    }
    let mut total = 0;
    for_each_stacktrace_mut(ev, |holder, keep_raw| {
        // Copy the original frames only when one of them can change.
        let raw = (keep_raw
            && holder.get("raw_stacktrace").is_none()
            && frames(holder)
                .any(|f| is_symbolicatable(f) && f["abs_path"].as_str().is_some_and(|abs| maps.contains_key(abs))))
        .then(|| holder.get("stacktrace").cloned())
        .flatten();
        let mut changed = 0;
        if let Some(frames) =
            holder.get_mut("stacktrace").and_then(|s| s.get_mut("frames")).and_then(Value::as_array_mut)
        {
            for f in frames.iter_mut().filter(|f| is_symbolicatable(f)) {
                let rm = f["abs_path"].as_str().and_then(|abs| maps.get(abs)).cloned();
                if let Some(rm) = rm
                    && rewrite_frame(f, &rm)
                {
                    changed += 1;
                }
            }
        }
        if changed > 0
            && let (Some(raw), Some(o)) = (raw, holder.as_object_mut())
        {
            o.insert("raw_stacktrace".into(), raw);
        }
        total += changed;
    });
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use sourcemap::SourceMapBuilder;

    /// Original `src/app.ts`:
    /// ```text
    /// 0: // header
    /// 1: export function handleClick(user) {
    /// 2:   return user.name.toUpperCase();
    /// 3: }
    /// ```
    /// Minified: `function n(e){return e.name.toUpperCase()}` on line 0.
    fn fixture() -> ResolvedSourceMap {
        let minified = "function n(e){return e.name.toUpperCase()}";
        let mut b = SourceMapBuilder::new(Some("main.js"));
        let src = b.add_source("../src/app.ts");
        b.set_source_contents(
            src,
            Some("// header\nexport function handleClick(user) {\n  return user.name.toUpperCase();\n}\n"),
        );
        b.add_raw(0, 0, 1, 7, Some(src), None, false); // "function"
        let name = b.add_name("handleClick");
        b.add_raw(0, 9, 1, 16, Some(src), Some(name), false); // "n" -> handleClick
        let user = b.add_name("user");
        b.add_raw(0, 11, 1, 28, Some(src), Some(user), false); // "e" -> user
        b.add_raw(0, 21, 2, 9, Some(src), Some(user), false); // "e.name" -> user.name
        let sm = b.into_sourcemap();
        ResolvedSourceMap {
            map: Arc::new(sm),
            minified: Some(Arc::new(SourceView::new(Arc::from(minified)))),
            map_url: "https://cdn.test/assets/main.js.map".into(),
            resolved_with: "debug-id",
        }
    }

    fn event() -> Value {
        json!({
            "platform": "javascript",
            "debug_meta": {"images": [{"type": "sourcemap", "code_file": "https://cdn.test/assets/main.js", "debug_id": "c941d872-af1f-4f0c-a7ff-ad3d295fe153"}]},
            "exception": {"values": [{
                "type": "TypeError",
                "value": "Cannot read properties of undefined (reading 'toUpperCase')",
                "stacktrace": {"frames": [
                    {"abs_path": "https://cdn.test/assets/main.js", "filename": "/assets/main.js", "function": "n", "lineno": 1, "colno": 22, "in_app": true},
                    {"abs_path": "https://other.test/x.js", "function": "z", "lineno": 1, "colno": 1}
                ]}
            }]}
        })
    }

    #[test]
    fn collect_refs_dedupes_and_stops_at_the_limit() {
        let frames: Vec<Value> = (0..100)
            .map(|i| json!({"abs_path": format!("https://h.test/{}.js", i % 10), "lineno": 1, "colno": 1}))
            .collect();
        let ev = json!({"stacktrace": {"frames": frames}});
        assert_eq!(collect_refs(&ev, 50).len(), 10);
        let refs = collect_refs(&ev, 3);
        let paths: Vec<&str> = refs.iter().map(|r| r.abs_path.as_str()).collect();
        assert_eq!(paths, ["https://h.test/0.js", "https://h.test/1.js", "https://h.test/2.js"]);
    }

    #[test]
    fn collects_refs_with_debug_ids() {
        let refs = collect_refs(&event(), 50);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].debug_id, Some(Uuid::parse_str("c941d872-af1f-4f0c-a7ff-ad3d295fe153").unwrap()));
        assert_eq!(refs[1].debug_id, None);
    }

    #[test]
    fn rewrites_frames_and_keeps_raw() {
        let mut ev = event();
        let mut maps = HashMap::new();
        maps.insert("https://cdn.test/assets/main.js".to_string(), Arc::new(fixture()));
        assert_eq!(apply(&mut ev, &maps), 1);
        let exc = &ev["exception"]["values"][0];
        let f = &exc["stacktrace"]["frames"][0];
        assert_eq!(f["lineno"], 3);
        assert_eq!(f["colno"], 10);
        assert_eq!(f["abs_path"], "https://cdn.test/src/app.ts");
        assert_eq!(f["filename"], "/src/app.ts");
        assert_eq!(f["function"], "handleClick");
        assert_eq!(f["context_line"], "  return user.name.toUpperCase();");
        assert_eq!(f["pre_context"][1], "export function handleClick(user) {");
        assert_eq!(f["data"]["symbolicated"], true);
        assert_eq!(exc["raw_stacktrace"]["frames"][0]["function"], "n");
        // Unresolved frame untouched; applying twice is a no-op.
        assert_eq!(exc["stacktrace"]["frames"][1]["function"], "z");
        assert_eq!(apply(&mut ev, &maps), 0);
    }

    #[test]
    fn out_of_range_line_is_not_symbolicated() {
        let mut ev = event();
        // 2^32 + 1 would wrap to line 0 with a truncating cast.
        ev["exception"]["values"][0]["stacktrace"]["frames"][0]["lineno"] = json!(4_294_967_297u64);
        let mut maps = HashMap::new();
        maps.insert("https://cdn.test/assets/main.js".to_string(), Arc::new(fixture()));
        assert_eq!(apply(&mut ev, &maps), 0);
        assert!(ev["exception"]["values"][0].get("raw_stacktrace").is_none());
    }

    #[test]
    fn context_at_the_edges() {
        let mut b = SourceMapBuilder::new(None);
        let src = b.add_source("a.js");
        b.set_source_contents(src, Some("l0\r\nl1\nl2\nl3\nl4\nl5\nl6\nl7"));
        let sm = b.into_sourcemap();
        let (pre, cur, post) = context(&sm, src, 0).unwrap();
        assert!(pre.is_empty());
        assert_eq!(cur, "l0");
        assert_eq!(post, vec![json!("l1"), json!("l2"), json!("l3"), json!("l4"), json!("l5")]);
        let (pre, cur, post) = context(&sm, src, 7).unwrap();
        assert_eq!(pre, vec![json!("l2"), json!("l3"), json!("l4"), json!("l5"), json!("l6")]);
        assert_eq!(cur, "l7");
        assert!(post.is_empty());
        assert!(context(&sm, src, 8).is_none());
    }

    #[test]
    fn webpack_sources() {
        assert_eq!(resolve_source("webpack://my-app/./src/a.ts", "x").1, "src/a.ts");
        assert_eq!(resolve_source("../src/a.ts", "https://h.test/assets/m.js.map").0, "https://h.test/src/a.ts");
    }

    #[test]
    fn candidates() {
        assert_eq!(
            url_candidates("https://h.test/static/app.js?v=2"),
            vec!["https://h.test/static/app.js?v=2", "~/static/app.js?v=2", "~/static/app.js"]
        );
        assert_eq!(url_candidates("/srv/app/dist/index.js"), vec!["/srv/app/dist/index.js", "~/srv/app/dist/index.js"]);
    }
}
