//! The direction of dependencies between the top-level modules of `argus-core`, and
//! which modules must stay pure. A module may use only modules in earlier layers, so
//! the graph has no cycles and a change low in the stack never needs one high up.
//! Test code (`#[cfg(test)]` items) is not checked: it may reach anything to set up a
//! case. It also checks the crate graph of the workspace in Cargo.lock.
//!
//! See docs/design/RATIONALE.md ("Shape") for what each module is for.

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
    &["fact", "command"],
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

/// Which workspace crates each crate may depend on (normal, dev, or build). The
/// workspace is std-only; argus-app, which needs Tauri, is outside it.
const CRATE_DEPS: &[(&str, &[&str])] = &[
    ("argus-log", &[]),
    ("argus-json", &[]),
    ("argus-core", &["argus-log", "argus-json"]),
    // View models are presentation shapes, not domain types.
    ("argus-api", &["argus-json"]),
    (
        "argus-host",
        &["argus-log", "argus-json", "argus-core", "argus-api"],
    ),
    // The CLI is a client of the API like the app and the phone; it never opens a ledger.
    ("argus-cli", &["argus-json", "argus-api"]),
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
            violations.push(format!(
                "src/{rel}:{line}: `{from}` (layer {}) uses `{path}` (layer {})",
                rank(from).unwrap(),
                rank(to).unwrap()
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{} uses point up or sideways in the layers of argus-core:\n  {}\n\n\
         A module may use only modules in earlier layers (LAYERS in crates/argus-core/tests/architecture.rs).\n\
         Fix the design, not the check: move what both modules need down into the lower module (or into `ids`), \
         or let the caller pass it in as a value or a function. Do not re-export the item to hide the use.",
        violations.len(),
        violations.join("\n  ")
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
fn workspace_crates_depend_only_on_allowed_crates() {
    let lock_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap();
    let problems = check_lock(&lock, CRATE_DEPS);
    assert!(
        problems.is_empty(),
        "Cargo.lock breaks the crate graph (CRATE_DEPS in crates/argus-core/tests/architecture.rs):\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_lock_check_names_third_party_crates_and_unlisted_edges() {
    let lock = r#"
[[package]]
name = "argus-cli"
version = "0.0.0"
dependencies = [
 "argus-core",
 "serde",
]

[[package]]
name = "argus-core"
version = "0.0.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    let problems = check_lock(lock, &[("argus-cli", &[]), ("argus-core", &[])]);
    assert_eq!(problems.len(), 3, "{problems:#?}");
    assert!(problems[0].starts_with("argus-cli depends on argus-core"));
    assert!(problems[1].starts_with("argus-cli depends on third-party crate serde"));
    assert!(problems[2].starts_with("third-party crate serde is in Cargo.lock"));
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

/// Each broken rule in a Cargo.lock, with the fix.
fn check_lock(lock: &str, allowed: &[(&str, &[&str])]) -> Vec<String> {
    let mut packages: Vec<(String, bool, Vec<String>)> = Vec::new();
    let mut in_deps = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            packages.push((String::new(), false, Vec::new()));
            in_deps = false;
        } else if let Some(package) = packages.last_mut() {
            if let Some(name) = line.strip_prefix("name = ") {
                package.0 = name.trim_matches('"').to_string();
            } else if line.starts_with("source = ") {
                package.1 = true;
            } else if line == "dependencies = [" {
                in_deps = true;
            } else if in_deps && line == "]" {
                in_deps = false;
            } else if in_deps {
                let dep = line.trim_end_matches(',').trim_matches('"');
                let dep = dep.split(' ').next().unwrap_or(dep);
                package.2.push(dep.to_string());
            }
        }
    }
    let third_party: Vec<&str> = packages
        .iter()
        .filter(|p| p.1)
        .map(|p| p.0.as_str())
        .collect();
    let mut problems = Vec::new();
    for (name, _, deps) in &packages {
        let Some((_, ok)) = allowed.iter().find(|(crate_name, _)| crate_name == name) else {
            continue;
        };
        for dep in deps {
            if third_party.contains(&dep.as_str()) {
                problems.push(format!(
                    "{name} depends on third-party crate {dep}. The workspace is std-only: \
                     write the part you need, or put code that needs {dep} in argus-app."
                ));
            } else if !ok.contains(&dep.as_str()) {
                problems.push(format!(
                    "{name} depends on {dep}, which CRATE_DEPS does not allow. \
                     Reach it through a crate {name} may use (argus-cli and the apps call argus-api), \
                     or move the shared part down into argus-json or argus-log."
                ));
            }
        }
    }
    for dep in third_party {
        problems.push(format!(
            "third-party crate {dep} is in Cargo.lock. Remove it from the workspace members; \
             only argus-app (outside the workspace) may use crates.io."
        ));
    }
    for (name, _, _) in &packages {
        let listed = allowed.iter().any(|(crate_name, _)| crate_name == name);
        if !listed && name.starts_with("argus-") {
            problems.push(format!(
                "{name} is not in CRATE_DEPS. Add it with the crates it may depend on."
            ));
        }
    }
    problems
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
