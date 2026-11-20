//! TLA+ model checking through java TLC or tlars. Both speak `-tool` mode, so the
//! command differs and the decoder does not.

use crate::event::{CheckRequest, CheckSpec, TlaEngine};
use crate::ids::{FactId, RepoPath};
use crate::plan::{CommandSpec, PlanEnv, Workdir};
use crate::source::{Decoded, Observation, SourceKind, TlcMessage};
use crate::views::checks::CheckVerdict;

pub(crate) const VERSION: u32 = 1;

/// JavaTlc: `java -XX:+UseParallelGC -cp <tla2tools.jar> tlc2.TLC -tool [-workers N]
///           [-config C.cfg] Spec.tla`
/// Tlars:   `tlars -tool [-workers N] [-config C.cfg] Spec.tla`
/// Workdir: `Snapshot { tree, base }`. stdout: `TlcTool`. stderr: `PlainText`.
///
/// Runs in the spec's directory (as `tools/tla-check` does), so the spec and its modules
/// are found the way TLC finds them when run by hand; a config elsewhere is named
/// relative to it. `-noGenerateSpecTE`: the trace is in the message stream already.
pub(crate) fn command(req: &CheckRequest, env: &PlanEnv) -> CommandSpec {
    let CheckSpec::Tla {
        engine,
        spec,
        config,
        workers,
    } = &req.spec
    else {
        unreachable!("plan() dispatches Tla only")
    };
    let (dir, file) = match spec.as_str().rsplit_once('/') {
        Some((dir, file)) => (RepoPath::parse(dir), file),
        None => (None, spec.as_str()),
    };
    let (program, mut args): (_, Vec<String>) = match engine {
        TlaEngine::JavaTlc => (
            env.tools.java.clone(),
            vec![
                "-XX:+UseParallelGC".into(),
                "-cp".into(),
                env.tools.tla2tools_jar.to_string_lossy().into_owned(),
                "tlc2.TLC".into(),
            ],
        ),
        TlaEngine::Tlars => (env.tools.tlars.clone(), Vec::new()),
    };
    args.extend(["-tool".into(), "-noGenerateSpecTE".into()]);
    if let Some(n) = workers {
        args.extend(["-workers".into(), n.to_string()]);
    }
    if let Some(config) = config {
        args.extend(["-config".into(), relative(dir.as_ref(), config)]);
    }
    args.push(file.to_owned());
    CommandSpec {
        program,
        args,
        env: Vec::new(),
        workdir: Workdir::Snapshot {
            tree: req.tree.clone(),
            base: req.base.clone(),
        },
        cwd: dir,
        stdin: None,
        stdout: SourceKind::TlcTool,
        stderr: SourceKind::PlainText,
        terminal: false,
        watch: Vec::new(),
        artifacts: false,
        timeout: None,
    }
}

/// `to` as seen from directory `from` (`None`: the repository root).
pub(crate) fn relative(from: Option<&RepoPath>, to: &RepoPath) -> String {
    let from: Vec<&str> = from.map_or(Vec::new(), |d| d.as_str().split('/').collect());
    let to: Vec<&str> = to.as_str().split('/').collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; from.len() - common];
    parts.extend(&to[common..]);
    parts.join("/")
}

/// One `@!@!@STARTMSG code:class @!@!@ ... @!@!@ENDMSG code @!@!@` block (the framer
/// cut it), or a stray line -> `Line`.
pub(crate) fn decode(bytes: &[u8]) -> Decoded {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let Some(rest) = text.strip_prefix("@!@!@STARTMSG ") else {
        return Decoded::one(Observation::Line(text.to_owned()));
    };
    match block(rest) {
        Some(message) => Decoded::one(Observation::Tlc(message)),
        None => Decoded::one(Observation::Unrecognized {
            reason: "malformed TLC message block".into(),
        }),
    }
}

/// `code:class @!@!@\n<body>\n@!@!@ENDMSG code @!@!@`, after the `STARTMSG ` prefix.
fn block(rest: &str) -> Option<TlcMessage> {
    let (head, body) = rest.split_once(" @!@!@\n")?;
    let (code, class) = head.split_once(':')?;
    let (code, class): (u32, u8) = (code.parse().ok()?, class.parse().ok()?);
    let body = body.strip_suffix(&format!("@!@!@ENDMSG {code} @!@!@"))?;
    let text = body.strip_suffix('\n').unwrap_or(body);
    Some(TlcMessage {
        code,
        class,
        text: text.trim_end_matches('\n').to_owned(),
    })
}

/// `MP` message classes (`tlars-msg`, generated from TLC's `MP.java`).
const CLASS_ERROR: u8 = 1;
const CLASS_TLC_BUG: u8 = 2;
const CLASS_STATE: u8 = 4;
/// `EC.TLC_SUCCESS`.
const TLC_SUCCESS: u32 = 2193;

/// Verdict from the message stream and exit code. Pure; used by `views::checks`.
///
/// The exit code says what kind of failure it was (`EC.ExitStatus`: 10 assumption,
/// 11 deadlock, 12 safety, 13 liveness, 14 assert); the messages say what and where. Both
/// must agree, or the verdict is `ToolError` (this also catches tlars/TLC divergence):
/// - exit 0, `TLC_SUCCESS`, no error message -> `Proved`.
/// - exit 10..=14 and an error message -> `Refuted`: the first error's text, and every
///   state message (the behavior up to the violation) as the trace.
/// - anything else -> `ToolError` with the first error's text.
pub(crate) fn verdict(messages: &[(FactId, TlcMessage)], exit_code: i32) -> CheckVerdict {
    let first_error = messages
        .iter()
        .find(|(_, m)| matches!(m.class, CLASS_ERROR | CLASS_TLC_BUG))
        .map(|(_, m)| m.text.clone());
    let succeeded = messages.iter().any(|(_, m)| m.code == TLC_SUCCESS);
    match (exit_code, first_error) {
        (0, None) if succeeded => CheckVerdict::Proved,
        (10..=14, Some(summary)) => CheckVerdict::Refuted {
            summary,
            trace: messages
                .iter()
                .filter(|(_, m)| m.class == CLASS_STATE)
                .map(|(f, _)| f.clone())
                .collect(),
        },
        (0, Some(error)) => CheckVerdict::ToolError {
            summary: format!("TLC exited 0 but reported: {error}"),
        },
        (0, None) => CheckVerdict::ToolError {
            summary: "TLC exited 0 without reporting success".into(),
        },
        (code, error) => CheckVerdict::ToolError {
            summary: error.unwrap_or_else(|| format!("TLC exited {code}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(code: u32, class: u8) -> (FactId, TlcMessage) {
        let fact = FactId::parse("src-0f8fad5d-d9cb-469f-a165-70867728950e:0.0").unwrap();
        let text = format!("message {code}");
        (fact, TlcMessage { code, class, text })
    }

    /// Exit 0 alone is not a proof: TLC must also have said it succeeded.
    #[test]
    fn success_needs_both_the_exit_code_and_the_success_message() {
        let finished = [message(2185, 0), message(2193, 0), message(2186, 0)];
        assert_eq!(verdict(&finished, 0), CheckVerdict::Proved);
        let silent = [message(2185, 0), message(2186, 0)];
        assert!(matches!(
            verdict(&silent, 0),
            CheckVerdict::ToolError { .. }
        ));
    }
}
