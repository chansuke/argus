//! Lean 4 via lake.

use argus_json::Json;

use crate::event::{CheckRequest, CheckSpec, LeanMode};
use crate::ids::FactId;
use std::path::PathBuf;

use crate::adapters::tlc;
use crate::plan::{CommandSpec, PlanEnv, Workdir};
use crate::source::CheckVerdict;
use crate::source::{Decoded, LeanMessage, LeanSeverity, Observation, SourceKind};

pub(crate) const VERSION: u32 = 1;

/// `LakeBuild`: `lake build` in `root` (stdout `LeanText`).
/// `Files`: `lake env lean --json <file>` per file, run sequentially by a wrapper script
/// (stdout `LeanJson`).
///
/// Both run in the package directory, so the paths Lean prints are package-relative
/// (`Checks::located` adds the root back). `Files` exits with the largest exit code of
/// its files, so one failing file fails the check.
pub(crate) fn command(req: &CheckRequest, env: &PlanEnv) -> CommandSpec {
    let CheckSpec::Lean { mode, root } = &req.spec else {
        unreachable!("plan() dispatches Lean only")
    };
    let (program, args, vars, stdout) = match mode {
        LeanMode::LakeBuild => (
            env.tools.lake.clone(),
            vec!["build".to_owned()],
            Vec::new(),
            SourceKind::LeanText,
        ),
        LeanMode::Files(files) => {
            let mut args = vec!["-c".to_owned(), FILES_SCRIPT.to_owned(), "sh".to_owned()];
            args.extend(files.iter().map(|f| tlc::relative(Some(root), f)));
            let lake = (
                "LAKE".to_owned(),
                env.tools.lake.to_string_lossy().into_owned(),
            );
            (PathBuf::from("sh"), args, vec![lake], SourceKind::LeanJson)
        }
    };
    CommandSpec {
        program,
        args,
        env: vars,
        workdir: Workdir::Snapshot {
            tree: req.tree.clone(),
            base: req.base.clone(),
        },
        cwd: Some(root.clone()),
        stdin: None,
        stdout,
        stderr: SourceKind::PlainText,
        terminal: false,
        watch: Vec::new(),
        artifacts: false,
        timeout: None,
    }
}

/// `sh -c FILES_SCRIPT sh <file>...`: every file is checked, and the worst exit is kept.
const FILES_SCRIPT: &str = r#"worst=0
for f do
  "$LAKE" env lean --json "$f"
  code=$?
  [ "$code" -gt "$worst" ] && worst=$code
done
exit "$worst""#;

/// One frame of `lake build` output: `severity: path:line:col: text` plus the lines under
/// it, as lake 4.35 prints them. Lake's own lines, and diagnostics without a location
/// (`error: no such file ...`), are `Line`s.
pub(crate) fn decode_text(bytes: &[u8]) -> Decoded {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let message = text.split_once(": ").and_then(|(severity, rest)| {
        let severity = severity_of(severity)?;
        let mut parts = rest.splitn(4, ':');
        let path = parts.next()?;
        let line = parts.next()?.parse().ok()?;
        let col = parts.next()?.parse().ok()?;
        let text = parts.next()?.strip_prefix(' ')?;
        path.ends_with(".lean").then(|| LeanMessage {
            path: path.to_owned(),
            line,
            col,
            severity,
            text: text.to_owned(),
        })
    });
    Decoded::one(match message {
        Some(m) => Observation::Lean(m),
        None => Observation::Line(text.to_owned()),
    })
}

/// One `lean --json` line: `{"severity","fileName","pos":{"line","column"},"data",...}`.
pub(crate) fn decode_json(bytes: &[u8]) -> Decoded {
    let message = argus_json::parse(bytes.trim_ascii_end())
        .ok()
        .and_then(|j| {
            let pos = j.get("pos")?;
            let num = |v: Option<&Json>| match v {
                Some(Json::Num(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
                _ => None,
            };
            Some(LeanMessage {
                path: j.get("fileName")?.as_str()?.to_owned(),
                line: num(pos.get("line"))?,
                col: num(pos.get("column"))?,
                severity: severity_of(j.get("severity")?.as_str()?)?,
                text: j.get("data")?.as_str()?.to_owned(),
            })
        });
    Decoded::one(match message {
        Some(m) => Observation::Lean(m),
        None => Observation::Unrecognized {
            reason: "not a lean --json message".into(),
        },
    })
}

fn severity_of(word: &str) -> Option<LeanSeverity> {
    match word {
        "error" => Some(LeanSeverity::Error),
        "warning" => Some(LeanSeverity::Warning),
        // Lean's JSON names it in full; unverified, so both spellings are read.
        "info" | "information" => Some(LeanSeverity::Info),
        _ => None,
    }
}

/// - located errors -> `Refuted`: the first error as `path:line:col: first line`, every
///   error as the trace. A proof that does not check is a refutation of the head.
/// - a non-zero exit without a located error (lake itself failed) -> `ToolError`.
/// - exit 0 with errors -> `ToolError` (the two disagree).
/// - otherwise "declaration uses `sorry`" warnings -> `Holes { sorries }` (green build,
///   unfinished proof: never shown as proved), or `Proved`.
pub(crate) fn verdict(messages: &[(FactId, LeanMessage)], exit_code: i32) -> CheckVerdict {
    let errors: Vec<&(FactId, LeanMessage)> = messages
        .iter()
        .filter(|(_, m)| m.severity == LeanSeverity::Error)
        .collect();
    match (exit_code, errors.first()) {
        (0, Some(_)) => CheckVerdict::ToolError {
            summary: "lake exited 0 but reported errors".into(),
        },
        (_, Some((_, first))) => CheckVerdict::Refuted {
            summary: format!(
                "{}:{}:{}: {}",
                first.path,
                first.line,
                first.col,
                first.text.lines().next().unwrap_or_default()
            ),
            trace: errors.iter().map(|(f, _)| f.clone()).collect(),
        },
        (0, None) => {
            let sorries = messages
                .iter()
                .filter(|(_, m)| m.severity == LeanSeverity::Warning)
                .filter(|(_, m)| m.text.starts_with("declaration uses `sorry`"))
                .count() as u32;
            match sorries {
                0 => CheckVerdict::Proved,
                sorries => CheckVerdict::Holes { sorries },
            }
        }
        (code, None) => CheckVerdict::ToolError {
            summary: format!("lake exited {code}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_severity_lean_writes_is_read() {
        let severity = |word: &str| {
            let line = format!(
                r#"{{"severity":"{word}","fileName":"A.lean","pos":{{"line":1,"column":0}},"data":"d"}}"#
            );
            match decode_json(line.as_bytes()).iter().next() {
                Some(Observation::Lean(m)) => Some(m.severity),
                _ => None,
            }
        };
        assert_eq!(severity("error"), Some(LeanSeverity::Error));
        assert_eq!(severity("warning"), Some(LeanSeverity::Warning));
        assert_eq!(severity("information"), Some(LeanSeverity::Info));
        assert_eq!(severity("info"), Some(LeanSeverity::Info));
        assert_eq!(severity("loud"), None);
    }
}
