//! Protocol-agnostic version ordering.
//!
//! Several adapters serve a "latest" label (`npm dist-tags.latest`, Go
//! `@latest`, Maven `maven-metadata <latest>`, …). The substrate stores
//! versions as plain strings sorted lexically, which is wrong for semantic
//! versions (`1.0.10` > `1.0.9` lexically, `2.0.0` < `10.0.0`). This helper
//! picks the highest *semantic* version instead.
//!
//! Parsing is lenient to cover the cross-protocol flavors that the real
//! clients send:
//! - a leading `v`/`V` (Go `v1.2.3`);
//! - missing patch/minor (`1`, `1.2`);
//! - prerelease / build (`1.2.3-alpha.1+build`).
//!
//! Only strings that actually parse are compared semantically; anything else
//! falls back to a tie so callers keep the existing lexical-backstop behavior.

use semver::Version;
use std::cmp::Ordering as Cmp;

/// Normalize a version string into a semver [`Version`], if parseable.
/// Accepts an optional leading `v`/`V` and pads short triples.
pub fn try_parse(raw: &str) -> Option<Version> {
    let s = raw.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    if s.is_empty() {
        return None;
    }

    // Pad `1` -> `1..0`, `1.2` -> `1.2.0`.
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let dot_count = core.chars().filter(|c| *c == '.').count();
    let padded = match dot_count {
        0 => format!("{s}.0.0"),
        1 => format!("{s}.0"),
        _ => s.to_string(),
    };

    Version::parse(&padded).ok()
}

/// Highest semantic version among `versions` (empty -> `None`).
/// Non-parseable entries are ignored.
pub fn highest<'a, I>(versions: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a String>,
{
    let mut best: Option<(&str, Version)> = None;
    for v in versions {
        let Some(p) = try_parse(v) else { continue };
        match &best {
            Some((_, b)) if p <= *b => {}
            _ => best = Some((v.as_str(), p)),
        }
    }
    best.map(|(s, _)| s)
}

/// Highest semantic version among string slices (`&str` — protocol code often
/// iterates `Vec<String>` or `&str`).
pub fn highest_str<'a, I>(versions: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut best: Option<(&str, Version)> = None;
    for v in versions {
        let Some(p) = try_parse(v) else { continue };
        match &best {
            Some((_, b)) if p <= *b => {}
            _ => best = Some((v, p)),
        }
    }
    best.map(|(s, _)| s)
}

/// Compare two version strings semantically. Non-parseable entries are
/// compared lexically so the total order is stable.
pub fn compare(a: &str, b: &str) -> Cmp {
    match (try_parse(a), try_parse(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// Sort version strings in ascending semantic order (stable; non-parseable
/// strings fall back to their position determined by [`compare`]).
pub fn sort<'a, I>(versions: I) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut v: Vec<&str> = versions.into_iter().collect();
    v.sort_by(|a, b| compare(a, b));
    v
}

/// Copy of [`sort`] for owned string vectors: mutates in place using
/// [`compare`].
pub fn sort_vec(versions: &mut [String]) {
    versions.sort_by(|a, b| compare(a, b));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flavors() {
        assert!(try_parse("v1.2.3").is_some());
        assert!(try_parse("1.2.3-alpha.1+build").is_some());
        assert!(try_parse("1").is_some());
        assert!(try_parse("1.2").is_some());
        assert!(try_parse("not-a-version").is_none());
    }

    #[test]
    fn orders_semantically() {
        let vs = ["1.0.9".to_string(), "1.0.10".to_string(), "2.0.0".to_string()];
        assert_eq!(highest(vs.iter()), Some("2.0.0"));

        let vs2 = ["10.0.0".to_string(), "2.0.0".to_string()];
        assert_eq!(highest(vs2.iter()), Some("10.0.0"));

        let vs3 = ["2.0.0-rc.1".to_string(), "2.0.0".to_string()];
        assert_eq!(highest(vs3.iter()), Some("2.0.0"));

        // Go-style leading v.
        let vs4 = ["v1.0.0", "v1.1.0", "v1.0.10"];
        assert_eq!(highest_str(vs4.into_iter()), Some("v1.1.0"));
    }

    #[test]
    fn sorts_semantically() {
        let mut vs =
            ["1.0.10".to_string(), "2.0.0".to_string(), "1.0.9".to_string(), "10.0.0".to_string()];
        sort_vec(&mut vs);
        assert_eq!(
            vs.as_slice(),
            &["1.0.9".to_string(), "1.0.10".to_string(), "2.0.0".to_string(), "10.0.0".to_string()]
        );

        // Non-parseable fall back to lexical.
        let ordered = sort(["z", "1.0.0", "a"]);
        assert_eq!(ordered, vec!["1.0.0", "a", "z"]);
    }
}
