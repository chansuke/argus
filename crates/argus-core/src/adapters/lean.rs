//! Lean 4 via lake.

use argus_json::Json;

use crate::event::{CheckRequest, CheckSpec, LeanMode};
use crate::ids::FactId;
use crate::plan::{CommandSpec, PlanEnv};
use crate::source::{Decoded, LeanMessage, LeanSeverity, Observation};
use crate::views::checks::CheckVerdict;

pub(crate) const VERSION: u32 = 1;

/// `LakeBuild`: `lake build` in `root` (stdout `LeanText`).
/// `Files`: `lake env lean --json <file>` per file, run sequentially by a wrapper script
/// (stdout `LeanJson`).
pub(crate) fn command(req: &CheckRequest, env: &PlanEnv) -> CommandSpec {
    let CheckSpec::Lean { mode, .. } = &req.spec else {
        unreachable!("plan() dispatches Lean only")
    };
    match mode {
        LeanMode::LakeBuild => todo!("not implemented"),
        LeanMode::Files(_) => todo!("not implemented"),
    }
}

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
