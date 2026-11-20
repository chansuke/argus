//! Wildcards for file names: watch patterns (`tail::expand`) and the snapshot deny list
//! (`git::snapshot`).

/// `*` matches any run of characters; everything else matches itself.
pub(crate) fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => {
            let Some(name) = name.strip_prefix(head) else {
                return false;
            };
            (0..=name.len())
                .filter(|i| name.is_char_boundary(*i))
                .any(|i| wildcard(rest, &name[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_matches_any_run_within_a_name() {
        assert!(wildcard("*.pem", "key.pem"));
        assert!(wildcard(".env.*", ".env.local"));
        assert!(!wildcard(".env.*", ".env"));
        assert!(wildcard("id_*", "id_ed25519"));
        assert!(wildcard("*", ""));
        assert!(wildcard("a*b*c", "a-b-b-c"));
        assert!(!wildcard("*.pem", "key.pem.bak"));
        assert!(wildcard("é*", "été"));
    }
}
