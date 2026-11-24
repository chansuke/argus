//! The direction of dependencies between the top-level modules of `argus-core`, and
//! which modules must stay pure. A module may use only modules in earlier layers, so
//! the graph has no cycles and a change low in the stack never needs one high up.
//! Test code (`#[cfg(test)]` items) is not checked: it may reach anything to set up a
//! case.
//!
//! See docs/design/RATIONALE.md ("Shape") for what each module is for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Lowest layer first. A module may use modules of earlier layers only, never its own
/// layer or a later one. The groups L0-L5 are the coarse picture; inside a group the
/// order is the one the code needs.
const LAYERS: &[&[&str]] = &[
    // L0: values with no domain behavior.
    &["ids"],
    &["period", "models"],
    // L1: what is recorded.
    &["source"],
    &["diff"],
    &["event"],
    &["fact"],
    // L2: foreign formats in and commands out.
    &["adapters"],
    // L3: pure folds over facts.
    &["views"],
    // L4: decisions over views.
    &["policy", "plan"],
    // L5: storage.
    &["codec"],
    &["ledger"],
    &["store"],
];

/// Uses that point up or sideways today, as (file under src/, path written). This list
/// may only shrink: cut a cycle, then remove its lines here. Never add a line.
const KNOWN: &[(&str, &str)] = &[
    ("adapters/claude.rs", "crate::plan"),
    ("adapters/codex.rs", "crate::plan"),
    ("adapters/eas.rs", "crate::plan"),
    ("adapters/lean.rs", "crate::plan"),
    ("adapters/llm.rs", "crate::plan"),
    ("adapters/tlc.rs", "crate::plan"),
    ("codec.rs", "crate::ledger::ActorHeader"),
];

/// Modules that are pure functions of their inputs: replay must equal live, so they
/// read no file, clock, environment, thread, or process.
const PURE: &[&str] = &["views", "policy", "plan", "diff"];

/// Text that means IO or a clock, with the reason shown when it appears in a PURE module.
const IMPURE: &[(&str, &str)] = &[
    ("std::fs", "file IO"),
    ("std::env", "the environment"),
    ("std::thread", "threads"),
    ("std::process", "processes"),
    ("std::net", "the network"),
    ("SystemTime", "the clock"),
    ("Instant", "the clock"),
];

#[test]
fn modules_use_only_earlier_layers() {
    let src = src_dir();
    let files = rust_files(&src);
    let modules = top_level_modules(&src);

    let mut unknown: Vec<&str> = modules
        .iter()
        .map(String::as_str)
        .filter(|m| rank(m).is_none())
        .collect();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "These top-level modules of argus-core are not in LAYERS in crates/argus-core/tests/architecture.rs: {unknown:?}.\n\
         Put each in the earliest layer that holds everything it uses."
    );

    let mut found = BTreeSet::new();
    let mut violations = Vec::new();
    for file in &files {
        let rel = relative(&src, file);
        let Some(from) = owner(&rel) else { continue };
        let text = std::fs::read_to_string(file).unwrap();
        for (line, path) in crate_paths(&production(&text)) {
            let Some(to) = path.split("::").nth(1) else {
                continue;
            };
            if to == from || !modules.iter().any(|m| m == to) {
                continue;
            }
            let ok = matches!((rank(from), rank(to)), (Some(f), Some(t)) if t < f);
            if ok {
                continue;
            }
            found.insert((rel.clone(), path.clone()));
            if !KNOWN.contains(&(rel.as_str(), path.as_str())) {
                violations.push(format!(
                    "src/{rel}:{line}: `{from}` (layer {}) uses `{path}` (layer {})",
                    rank(from).unwrap(),
                    rank(to).unwrap()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{} uses point up or sideways in the layers of argus-core:\n  {}\n\n\
         A module may use only modules in earlier layers (LAYERS in crates/argus-core/tests/architecture.rs).\n\
         Fix the design, not the check: move what both modules need down into the lower module (or into `ids`), \
         or let the caller pass it in as a value or a function. Do not re-export the item to hide the use, \
         and do not add the use to KNOWN.",
        violations.len(),
        violations.join("\n  ")
    );

    let stale: Vec<_> = KNOWN
        .iter()
        .filter(|(file, path)| !found.contains(&(file.to_string(), path.to_string())))
        .collect();
    assert!(
        stale.is_empty(),
        "These KNOWN uses are gone. Remove them from KNOWN in crates/argus-core/tests/architecture.rs \
         so they cannot come back: {stale:?}"
    );
}

#[test]
fn pure_modules_do_no_io_and_read_no_clock() {
    let src = src_dir();
    let mut violations = Vec::new();
    for file in rust_files(&src) {
        let rel = relative(&src, &file);
        let Some(module) = owner(&rel) else { continue };
        if !PURE.contains(&module) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (index, line) in production(&text).lines().enumerate() {
            for (needle, what) in IMPURE {
                if contains_word(line, needle) {
                    violations.push(format!(
                        "src/{rel}:{}: `{module}` uses `{needle}` ({what})",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "{}\n\n\
         views, policy, plan, and diff are pure folds: no IO and no clock, so replay equals live. \
         Take the value as an argument instead (see `JobViews::glance(now)`), and let argus-host do the IO.",
        violations.join("\n")
    );
}

#[test]
fn the_scanner_skips_tests_and_comments_and_reads_full_paths() {
    let text = "\
use crate::ids::JobId;
// crate::views::Hidden is a comment
/// [`crate::views::Doc`] is a doc comment
fn f() { let _ = crate::adapters::llm::review_prompt(); }
#[cfg(test)]
mod tests {
    use crate::views::InTest;
}
fn g(_: crate::plan::CommandSpec) {}
";
    let found = crate_paths(&production(text));
    assert_eq!(
        found,
        vec![
            (1, "crate::ids::JobId".to_string()),
            (4, "crate::adapters::llm::review_prompt".to_string()),
            (9, "crate::plan::CommandSpec".to_string()),
        ]
    );
    assert!(contains_word("let t = Instant::now();", "Instant"));
    assert!(!contains_word("struct InstantReply;", "Instant"));
    assert!(contains_word("use std::fs::File;", "std::fs"));
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rank(module: &str) -> Option<usize> {
    LAYERS.iter().position(|layer| layer.contains(&module))
}

/// The top-level module a file under src/ belongs to; `None` for lib.rs, the facade.
fn owner(rel: &str) -> Option<&str> {
    if rel == "lib.rs" {
        return None;
    }
    let first = rel.split('/').next()?;
    Some(first.strip_suffix(".rs").unwrap_or(first))
}

fn relative(src: &Path, file: &Path) -> String {
    file.strip_prefix(src)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

fn top_level_modules(src: &Path) -> Vec<String> {
    let lib = std::fs::read_to_string(src.join("lib.rs")).unwrap();
    production(&lib)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("pub mod ")
                .or_else(|| line.strip_prefix("pub(crate) mod "))
                .or_else(|| line.strip_prefix("mod "))?;
            Some(rest.trim_end_matches(';').trim().to_string())
        })
        .collect()
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out
}

/// The text with comments blanked and every `#[cfg(test)]` item blanked, keeping line
/// numbers.
fn production(text: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    let mut depth = 0i64;
    let mut opened = false;
    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("");
        if !skipping && code.trim() == "#[cfg(test)]" {
            skipping = true;
            depth = 0;
            opened = false;
            out.push("");
            continue;
        }
        if skipping {
            for c in code.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';')) {
                skipping = false;
            }
            out.push("");
            continue;
        }
        out.push(code);
    }
    out.join("\n")
}

/// Every `crate::a::b::C` path in the text, with its 1-based line number.
fn crate_paths(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("crate::") {
            let before = rest[..at].chars().next_back();
            let tail = &rest[at..];
            let len = tail
                .char_indices()
                .find(|&(i, c)| !(c.is_alphanumeric() || c == '_' || c == ':') && i > 0)
                .map_or(tail.len(), |(i, _)| i);
            let path = tail[..len].trim_end_matches(':');
            if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                found.push((index + 1, path.to_string()));
            }
            rest = &tail[len..];
        }
    }
    found
}

/// Whether `needle` occurs in `line` with no identifier character on either side.
fn contains_word(line: &str, needle: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(needle).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + needle.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}
