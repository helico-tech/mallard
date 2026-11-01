//! Tiny User-Agent parser used to fill the `browser` and `os` contexts when
//! a browser SDK did not send them. It only needs to be good enough for tag
//! facets ("Chrome 130", "macOS"), not for analytics.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub browser: Option<Named>,
    pub os: Option<Named>,
}

/// A browser or operating system. `version` may be empty.
#[derive(Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub version: String,
}

static BROWSERS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("Edge", r"Edg(?:e|A|iOS)?/(\d+(?:\.\d+)?)"),
        ("Opera", r"OPR/(\d+(?:\.\d+)?)"),
        ("Samsung Internet", r"SamsungBrowser/(\d+(?:\.\d+)?)"),
        ("Firefox", r"(?:Firefox|FxiOS)/(\d+(?:\.\d+)?)"),
        ("Chrome", r"(?:Chrome|CriOS)/(\d+(?:\.\d+)?)"),
        ("Safari", r"Version/(\d+(?:\.\d+)?).*Safari/"),
    ]
    .into_iter()
    .map(|(n, r)| (n, Regex::new(r).expect("valid regex")))
    .collect()
});

static OSES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("iOS", r"(?:iPhone|iPad|iPod).*? OS (\d+(?:_\d+)?)"),
        ("Android", r"Android (\d+(?:\.\d+)?)"),
        ("Windows", r"Windows NT (\d+(?:\.\d+)?)"),
        ("macOS", r"Mac OS X (\d+(?:[._]\d+)?)"),
        ("Chrome OS", r"CrOS \S+ (\d+(?:\.\d+)?)"),
        ("Linux", r"Linux()"),
    ]
    .into_iter()
    .map(|(n, r)| (n, Regex::new(r).expect("valid regex")))
    .collect()
});

pub fn parse(ua: &str) -> Parsed {
    let find = |list: &[(&'static str, Regex)]| {
        list.iter().find_map(|(name, re)| {
            re.captures(ua).map(|c| Named {
                name: name.to_string(),
                version: c.get(1).map_or("", |m| m.as_str()).replace('_', "."),
            })
        })
    };
    Parsed { browser: find(&BROWSERS), os: find(&OSES) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, version: &str) -> Named {
        Named { name: name.into(), version: version.into() }
    }

    #[test]
    fn chrome_mac() {
        let p = parse(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
        );
        assert_eq!(p.browser, Some(named("Chrome", "130.0")));
        assert_eq!(p.os, Some(named("macOS", "10.15")));
    }

    #[test]
    fn safari_ios_and_firefox_linux() {
        let p = parse(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1",
        );
        assert_eq!(p.browser, Some(named("Safari", "17.4")));
        assert_eq!(p.os, Some(named("iOS", "17.4")));
        let p = parse("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0");
        assert_eq!(p.browser, Some(named("Firefox", "131.0")));
        assert_eq!(p.os, Some(named("Linux", "")));
    }

    #[test]
    fn edge_windows() {
        let p = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
        );
        assert_eq!(p.browser.unwrap().name, "Edge");
        assert_eq!(p.os.unwrap().name, "Windows");
    }
}
