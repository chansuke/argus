//! Which files an agent's tool call edits, read from the call itself. Shared by the agent
//! adapters; each knows its own tool names and calls these for the content.

use crate::diff::line_hash;
use crate::source::{FileTouch, TouchHow};

/// `path` gets the lines of `texts`, as hashes the provenance view matches hunks with.
pub(crate) fn inserted(path: Option<&str>, texts: &[&str]) -> Vec<FileTouch> {
    let Some(path) = path else { return Vec::new() };
    let hashes = texts
        .iter()
        .flat_map(|t| t.lines())
        .map(line_hash)
        .collect();
    vec![FileTouch {
        path: path.to_owned(),
        how: TouchHow::Inserted(hashes),
    }]
}

/// Words of a shell command that look like file paths. Coarse on purpose: a hunk this
/// names is "via shell" rather than unexplained; it never claims what was written.
pub(crate) fn shell_paths(command: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in command.split(|c: char| c.is_whitespace() || ";|&<>()`".contains(c)) {
        let word = word.trim_matches(|c| c == '\'' || c == '"');
        let path_chars = word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/~+-@".contains(c));
        let has_ext = word
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && !stem.ends_with('/'));
        let looks_like_path = !word.is_empty()
            && path_chars
            && !word.starts_with('-')
            && !word.ends_with('/')
            && (word.contains('/') || has_ext);
        if looks_like_path && !out.iter().any(|p| p == word) {
            out.push(word.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coarse on purpose, but each rule is pinned: a hunk a shell command names is "via
    /// shell" instead of unexplained.
    #[test]
    fn shell_commands_name_the_paths_they_touch() {
        let cases: [(&str, &[&str]); 9] = [
            ("sed -i s/a/b/ src/lib.rs", &["src/lib.rs"]),
            ("cargo fmt -- --check", &[]),
            ("rm -rf target/ && touch out.txt", &["out.txt"]),
            ("cat 'docs/a.md'|wc -l", &["docs/a.md"]),
            ("./run.sh;make", &["./run.sh"]),
            ("echo hello > .env", &[]),
            ("cp a.rs a.rs", &["a.rs"]),
            ("python -c x.y(1)", &["x.y"]),
            ("ls ~/notes/todo.md -o -f.txt", &["~/notes/todo.md"]),
        ];
        for (command, want) in cases {
            assert_eq!(shell_paths(command), want, "{command}");
        }
    }
}
