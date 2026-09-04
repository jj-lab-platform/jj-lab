//! Validation for org/repo path segments and git ref names (branch/tag).
//!
//! These rules are a security boundary: org/repo map directly onto filesystem
//! directories (`RepoStore::repo_dir`), so an unvalidated segment is a path
//! traversal; branch/tag names are exported to git refs by jj, so they must
//! satisfy git's `check-ref-format` (jj's `RefNameBuf` enforces most of it,
//! but we validate up-front for a clean 400 instead of a late 500).

/// Validate an org or repo name — a single path segment, never containing a
/// path separator or traversal.
pub fn validate_segment(name: &str, kind: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{kind} name must not be empty"));
    }
    if name.len() > 100 {
        return Err(format!("{kind} name is too long"));
    }
    if name == "." || name == ".." {
        return Err(format!("{kind} name is invalid"));
    }
    if name.eq_ignore_ascii_case(".git") {
        return Err(format!("{kind} name is reserved"));
    }
    if name.starts_with('.') {
        return Err(format!("{kind} name must not start with '.'"));
    }
    if name.ends_with('.') {
        return Err(format!("{kind} name must not end with '.'"));
    }
    // Only safe identity characters; explicitly excludes '/', '\\', control
    // chars, and anything else a filesystem or URL could misinterpret.
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !ok {
        return Err(format!(
            "{kind} name may only contain letters, digits, '-', '_', and '.'"
        ));
    }
    // Windows-device/legacy names are not worth accepting for a hosting layer.
    if let Some(base) = name.split('.').next() {
        if is_windows_reserved(base) {
            return Err(format!("{kind} name is reserved"));
        }
    }
    Ok(())
}

fn is_windows_reserved(base: &str) -> bool {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
        "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    RESERVED.iter().any(|r| base.eq_ignore_ascii_case(r))
}

/// Validate a branch or tag name against git `check-ref-format` semantics.
///
/// Slash `/` is allowed (e.g. `feat/log-optimization`); backslash, spaces,
/// `..`, `@{`, `~^:?*[`, control chars, and empty/`.`-prefixed/`.lock`-
/// suffixed segments are rejected — mirroring git's own rules.
pub fn validate_ref_name(name: &str, kind: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{kind} name must not be empty"));
    }
    if name.len() > 255 {
        return Err(format!("{kind} name is too long"));
    }
    if name.starts_with('-') {
        return Err(format!("{kind} name must not start with '-'"));
    }
    if name.ends_with(".lock") {
        return Err(format!("{kind} name must not end with '.lock'"));
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err(format!("{kind} name must not start or end with '/'"));
    }
    if name.contains("//") {
        return Err(format!("{kind} name contains an empty path segment"));
    }
    if name.contains("..") {
        return Err(format!("{kind} name must not contain '..'"));
    }
    if name.contains("@{") {
        return Err(format!("{kind} name must not contain '@{{'"));
    }
    if name == "@" {
        return Err(format!("{kind} name must not be '@'"));
    }
    if name.ends_with('.') {
        return Err(format!("{kind} name must not end with '.'"));
    }
    for seg in name.split('/') {
        if seg.is_empty() {
            return Err(format!("{kind} name contains an empty path segment"));
        }
        if seg.starts_with('.') {
            return Err(format!("{kind} name segment must not start with '.'"));
        }
    }
    // Disallowed chars (superset of git's): backslash, control, space, and the
    // ASCII symbols git forbids in refs.
    if name.chars().any(|c| {
        c == '\\'
            || c == ' '
            || c == '~'
            || c == '^'
            || c == ':'
            || c == '?'
            || c == '*'
            || c == '['
            || c.is_control()
    }) {
        return Err(format!("{kind} name contains forbidden characters"));
    }
    // Must contain at least one non-dot-implied printable char (above covers).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_accept_safe_names() {
        assert!(validate_segment("my-repo", "repo").is_ok());
        assert!(validate_segment("my.repo_1", "repo").is_ok());
        assert!(validate_segment("Org123", "org").is_ok());
    }

    #[test]
    fn segments_reject_traversal_and_separators() {
        assert!(validate_segment("..", "repo").is_err());
        assert!(validate_segment(".", "repo").is_err());
        assert!(validate_segment("a/b", "repo").is_err());
        assert!(validate_segment("a\\b", "repo").is_err());
        assert!(validate_segment(".git", "repo").is_err());
        assert!(validate_segment(".hidden", "repo").is_err());
        assert!(validate_segment("con", "repo").is_err());
        assert!(validate_segment("a b", "repo").is_err());
        assert!(validate_segment("", "repo").is_err());
    }

    #[test]
    fn refs_allow_slash_but_reject_backslash() {
        assert!(validate_ref_name("feat/log-optimization", "bookmark").is_ok());
        assert!(validate_ref_name("main", "bookmark").is_ok());
        assert!(validate_ref_name("v1.0.0", "tag").is_ok());
        assert!(validate_ref_name("a\\b", "bookmark").is_err());
        assert!(validate_ref_name("a b", "bookmark").is_err());
        assert!(validate_ref_name("a..b", "bookmark").is_err());
        assert!(validate_ref_name("@{", "bookmark").is_err());
    }

    #[test]
    fn refs_reject_edge_forms() {
        assert!(validate_ref_name("", "bookmark").is_err());
        assert!(validate_ref_name("/lead", "bookmark").is_err());
        assert!(validate_ref_name("trail/", "bookmark").is_err());
        assert!(validate_ref_name("a//b", "branch").is_err());
        assert!(validate_ref_name("a.lock", "bookmark").is_err());
        assert!(validate_ref_name(".dot", "bookmark").is_err());
        assert!(validate_ref_name("a.", "bookmark").is_err());
        assert!(validate_ref_name("-a", "bookmark").is_err());
        assert!(validate_ref_name("a:colon", "bookmark").is_err());
    }
}