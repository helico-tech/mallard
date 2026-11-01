//! The config loader: defaults, then a TOML file, then environment variables.
//!
//! - `<PREFIX><SECTION>__<KEY>=value` overrides one setting, e.g.
//!   `MALLARD_OTEL__RETENTION__TRACES=0`. For a setting that is a string,
//!   the value is kept as text (`0`, `123` and `true` stay strings). Other
//!   values are parsed as TOML (numbers, booleans, arrays). A list setting
//!   also takes a comma separated value: `..._API_TOKENS=a,b`.
//! - `${NAME}` inside a string value is replaced by environment variable
//!   NAME after the file is parsed, so the value may contain quotes,
//!   backslashes or newlines, and comments are never expanded.
//! - Unknown keys are errors when the config type uses `deny_unknown_fields`.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};

/// Load `T` from its defaults, the TOML file at `path` (optional) and the
/// process environment.
pub fn load<T: Serialize + DeserializeOwned + Default>(path: Option<&Path>, env_prefix: &str) -> Result<T> {
    let text = path
        .map(|p| std::fs::read_to_string(p).with_context(|| format!("reading config file {}", p.display())))
        .transpose()?;
    load_from(text.as_deref(), env_prefix, std::env::vars()).with_context(|| match path {
        Some(p) => format!("loading {}", p.display()),
        None => "loading the configuration".into(),
    })
}

/// [`load`] with the file text and the environment given explicitly.
pub fn load_from<T: Serialize + DeserializeOwned + Default>(
    file: Option<&str>,
    env_prefix: &str,
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<T> {
    let vars: HashMap<String, String> = vars.into_iter().collect();
    let mut root = toml::Value::try_from(T::default()).context("serializing the defaults")?;
    if let Some(text) = file {
        let file: toml::Value = toml::from_str(text).context("parsing TOML")?;
        merge(&mut root, file);
    }
    apply_env(&mut root, env_prefix, &vars)?;
    interpolate(&mut root, &|name| vars.get(name).cloned())?;
    root.try_into().context("invalid configuration")
}

fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) if bv.is_table() && v.is_table() => merge(bv, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Apply `<PREFIX>SECTION__KEY=value` overrides. Names without `__` (e.g.
/// `MALLARD_CONFIG`, or a variable only used through `${...}`) are skipped.
fn apply_env(root: &mut toml::Value, prefix: &str, vars: &HashMap<String, String>) -> Result<()> {
    let mut keys: Vec<&String> = vars.keys().filter(|k| k.starts_with(prefix)).collect();
    keys.sort(); // deterministic when two variables name the same setting
    for k in keys {
        let v = &vars[k];
        let path: Vec<String> = k[prefix.len()..].split("__").map(|p| p.to_ascii_lowercase()).collect();
        if path.len() < 2 {
            continue;
        }
        let existing = path.iter().try_fold(&*root, |cur, seg| cur.get(seg.as_str()));
        let parsed = || toml::from_str::<toml::Table>(&format!("v = {v}")).ok().and_then(|mut t| t.remove("v"));
        let value = match existing {
            // Keep the raw text for string settings (e.g. retention "0").
            Some(toml::Value::String(_)) => toml::Value::String(v.clone()),
            Some(toml::Value::Array(_)) => match parsed() {
                Some(a @ toml::Value::Array(_)) => a,
                _ => toml::Value::Array(
                    v.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(|p| toml::Value::String(p.into()))
                        .collect(),
                ),
            },
            _ => parsed().unwrap_or_else(|| toml::Value::String(v.clone())),
        };
        let mut cur = &mut *root;
        for (i, seg) in path.iter().enumerate() {
            let table = cur.as_table_mut().with_context(|| format!("env override {k}: {seg} is not a section"))?;
            if i == path.len() - 1 {
                table.insert(seg.clone(), value);
                break;
            }
            cur = table.entry(seg.clone()).or_insert_with(|| toml::Value::Table(Default::default()));
        }
    }
    Ok(())
}

/// Replace `${NAME}` in all string values.
fn interpolate(v: &mut toml::Value, lookup: &dyn Fn(&str) -> Option<String>) -> Result<()> {
    match v {
        toml::Value::String(s) => {
            let mut out = String::with_capacity(s.len());
            let mut rest = s.as_str();
            while let Some(start) = rest.find("${") {
                out.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let end = after.find('}').context("unterminated ${ in config value")?;
                let name = &after[..end];
                let val = lookup(name).with_context(|| format!("config references ${{{name}}} but it is not set"))?;
                out.push_str(&val);
                rest = &after[end + 1..];
            }
            out.push_str(rest);
            *s = out;
        }
        toml::Value::Array(a) => a.iter_mut().try_for_each(|x| interpolate(x, lookup))?,
        toml::Value::Table(t) => t.iter_mut().try_for_each(|(_, x)| interpolate(x, lookup))?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Cfg {
        server: Server,
        retention: Retention,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Server {
        public_url: String,
        api_tokens: Vec<String>,
        port: u16,
        enabled: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Retention {
        #[serde(with = "crate::duration")]
        traces: Duration,
    }

    impl Default for Cfg {
        fn default() -> Self {
            Cfg {
                server: Server { public_url: "http://localhost".into(), api_tokens: vec![], port: 80, enabled: true },
                retention: Retention { traces: Duration::from_secs(7 * 86_400) },
            }
        }
    }

    fn load(file: Option<&str>, vars: &[(&str, &str)]) -> Result<Cfg> {
        load_from(file, "APP_", vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn defaults_file_then_env() {
        let cfg = load(Some("[server]\nport = 81\nenabled = false"), &[("APP_SERVER__PORT", "82")]).unwrap();
        assert_eq!(cfg.server.port, 82);
        assert!(!cfg.server.enabled);
        assert_eq!(cfg.server.public_url, "http://localhost");
        assert_eq!(cfg.retention.traces, Duration::from_secs(7 * 86_400));
    }

    #[test]
    fn env_zero_stays_a_string() {
        // "0" is the documented way to keep data forever; it must not become a number.
        let cfg = load(None, &[("APP_RETENTION__TRACES", "0"), ("APP_SERVER__PUBLIC_URL", "123")]).unwrap();
        assert_eq!(cfg.retention.traces, Duration::ZERO);
        assert_eq!(cfg.server.public_url, "123");
    }

    #[test]
    fn env_single_token_and_lists() {
        let cfg = load(None, &[("APP_SERVER__API_TOKENS", "tok")]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["tok"]);
        let cfg = load(None, &[("APP_SERVER__API_TOKENS", "123")]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["123"], "a numeric token stays a string");
        let cfg = load(None, &[("APP_SERVER__API_TOKENS", "a, b,")]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["a", "b"]);
        let cfg = load(None, &[("APP_SERVER__API_TOKENS", r#"["x", "y"]"#)]).unwrap();
        assert_eq!(cfg.server.api_tokens, ["x", "y"]);
    }

    #[test]
    fn only_section_vars_override() {
        let cfg =
            load(None, &[("APP_CONFIG", "/etc/x.toml"), ("APP_TOKEN", "t"), ("OTHER_SERVER__PORT", "1")]).unwrap();
        assert_eq!(cfg.server.port, 80);
    }

    #[test]
    fn interpolation_happens_inside_parsed_strings() {
        let secret = "pa\"ss\\word\nline2 # not a comment";
        let file = "[server]\n# a comment with ${UNSET_VAR} is never expanded\napi_tokens = [\"${TOK}\"] # ${ALSO_UNSET}\npublic_url = \"https://${HOST}/x\"";
        let cfg = load(Some(file), &[("TOK", secret), ("HOST", "example.com")]).unwrap();
        assert_eq!(cfg.server.api_tokens, [secret]);
        assert_eq!(cfg.server.public_url, "https://example.com/x");
        // Env overrides are interpolated too.
        let cfg = load(None, &[("APP_SERVER__PUBLIC_URL", "${HOST}"), ("HOST", "h")]).unwrap();
        assert_eq!(cfg.server.public_url, "h");
    }

    #[test]
    fn errors() {
        let err = |file: &str, vars: &[(&str, &str)]| format!("{:#}", load(Some(file), vars).unwrap_err());
        assert!(err("[server]\npublic_url = \"${NOPE}\"", &[]).contains("NOPE"));
        assert!(err("[server]\npublic_url = \"${NOPE\"", &[]).contains("unterminated"));
        assert!(err("[server]\ntypo = 1", &[]).contains("typo"), "unknown keys are rejected");
        assert!(err("[nope]\nx = 1", &[]).contains("nope"));
        assert!(err("", &[("APP_SERVER__TYPO", "1")]).contains("typo"), "unknown env keys too");
        assert!(err("", &[("APP_SERVER__PUBLIC_URL__X", "1")]).contains("not a section"));
        assert!(err("[server\n", &[]).contains("TOML"));
    }
}
