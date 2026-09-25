//! Session and service names are DNS labels so phase 2 can route
//! `<service>.<session>.localhost`.

const MAX_LABEL_LEN: usize = 63;

/// Lowercases `raw` and collapses every run of non-`[a-z0-9]` characters into
/// one hyphen, trimming hyphens at both ends and capping at 63 characters.
/// Returns `"session"` when nothing usable remains.
pub fn sanitize_label(raw: &str) -> String {
    let mut label = String::new();
    let mut pending_hyphen = false;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if pending_hyphen && !label.is_empty() {
                label.push('-');
            }
            pending_hyphen = false;
            label.push(ch);
        } else {
            pending_hyphen = true;
        }
    }
    label.truncate(MAX_LABEL_LEN);
    let label = label.trim_end_matches('-');
    if label.is_empty() {
        "session".to_string()
    } else {
        label.to_string()
    }
}

/// Whether `value` is a lowercase DNS label: `[a-z0-9-]`, 1–63 characters,
/// no leading or trailing hyphen.
pub fn is_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL_LEN
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Returns `base`, or `base-2`, `base-3`, … — the first not in `taken`, kept
/// within 63 characters. `base` must already be a sanitized (ASCII) label.
pub fn dedupe_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let keep = MAX_LABEL_LEN - suffix.len();
            let stem = base[..base.len().min(keep)].trim_end_matches('-');
            format!("{stem}{suffix}")
        })
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .expect("unbounded numeric suffixes always yield a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_to_a_lowercase_dns_label() {
        assert_eq!(sanitize_label("Feature_X"), "feature-x");
        assert_eq!(sanitize_label("  --My  Branch!!--"), "my-branch");
        assert_eq!(sanitize_label("ünïcode"), "n-code");
        assert_eq!(sanitize_label("___"), "session");
        let long = sanitize_label(&"a".repeat(100));
        assert_eq!(long.len(), 63);
        assert!(is_dns_label(&long));
    }

    #[test]
    fn truncation_never_leaves_a_trailing_hyphen() {
        let raw = format!("{}-b", "a".repeat(62));
        let label = sanitize_label(&raw);
        assert_eq!(label, "a".repeat(62));
    }

    #[test]
    fn recognizes_dns_labels() {
        assert!(is_dns_label("web"));
        assert!(is_dns_label("web-2"));
        assert!(!is_dns_label(""));
        assert!(!is_dns_label("-web"));
        assert!(!is_dns_label("web-"));
        assert!(!is_dns_label("Web"));
        assert!(!is_dns_label("web_2"));
        assert!(!is_dns_label(&"a".repeat(64)));
    }

    #[test]
    fn dedupes_with_numeric_suffixes() {
        assert_eq!(dedupe_name("app", &[]), "app");
        assert_eq!(dedupe_name("app", &["app"]), "app-2");
        assert_eq!(dedupe_name("app", &["app", "app-2"]), "app-3");
    }

    #[test]
    fn dedupe_keeps_long_names_within_63_characters() {
        let base = "a".repeat(63);
        let name = dedupe_name(&base, &[base.as_str()]);
        assert_eq!(name.len(), 63);
        assert!(name.ends_with("-2"));
        assert!(is_dns_label(&name));
    }
}
