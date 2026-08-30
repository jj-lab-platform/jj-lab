//! Single-star glob matching, shared by the Conan search (`pkg/*`) and
//! Composer `list.json?filter=` endpoints.

/// Match `s` against `pattern` containing any number of `*` wildcards.
/// Case-insensitive.
pub fn glob_match(s: &str, pattern: &str) -> bool {
    let s = s.to_lowercase();
    let pattern = pattern.to_lowercase();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return s == pattern;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    // Start must match unless the pattern begins with '*'.
    if let Some(first) = parts.first() {
        if !first.is_empty() && !s.starts_with(first) {
            return false;
        }
    }
    let mut pos = parts.first().map(|p| p.len()).unwrap_or(0);
    for seg in &parts[1..] {
        if seg.is_empty() {
            continue;
        }
        match s[pos..].find(seg) {
            Some(i) => pos += i + seg.len(),
            None => return false,
        }
    }
    // Trailing segment (no trailing '*') must match the end.
    if let Some(last) = parts.last() {
        if !last.is_empty() && !pattern.ends_with('*') && !s.ends_with(last) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert!(glob_match("anything", "*"));
        assert!(glob_match("", "*"));
        assert!(glob_match("pkg/1.0", "pkg/*"));
        assert!(glob_match("pkg/1.0", "pkg/1.*"));
        assert!(!glob_match("pkg/1.0", "other/*"));
        assert!(glob_match("somewhere/pkg", "*pkg"));
        assert!(glob_match("conan/lib", "conan/*"));
    }
}
