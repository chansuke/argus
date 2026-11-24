//! Turning a run's purpose into a concrete invocation. Pure: no process is spawned here.
//! The host executes whatever comes back with one generic runner, so all knowledge of
//! external tools (flags, output formats, lease commands) stays in `adapters/`.

use crate::adapters;
use crate::command::{CommandSpec, Effect, Invocation, PermissionAnswer, PlanEnv, Workdir};
use crate::event::*;
use crate::ids::*;
use crate::models::AgentCli;
use crate::source::SourceKind;

/// The lease protocol steps of each provider; the adapter owns the commands.
impl LeaseProvider {
    pub fn parse_acquired(&self, stdout: &[u8]) -> Option<(String, Option<String>)> {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::parse_started(stdout),
        }
    }
    pub fn follow(&self, remote_id: &str, env: &PlanEnv) -> Option<CommandSpec> {
        match self {
            LeaseProvider::EasSimulator { .. } => {
                Some(adapters::eas::follow_events(remote_id, env))
            }
        }
    }
    pub fn collect(&self, remote_id: &str, env: &PlanEnv) -> Option<CommandSpec> {
        match self {
            LeaseProvider::EasSimulator { platform } => {
                adapters::eas::collect_logs(*platform, remote_id, env)
            }
        }
    }
    pub fn release(&self, remote_id: &str, env: &PlanEnv) -> CommandSpec {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::stop(remote_id, env),
        }
    }
    pub fn find(&self, name: &str, env: &PlanEnv) -> CommandSpec {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::find_by_tag(name, env),
        }
    }
    pub fn parse_found(&self, stdout: &[u8]) -> Vec<String> {
        match self {
            LeaseProvider::EasSimulator { .. } => adapters::eas::parse_list(stdout),
        }
    }
}

/// The keystrokes that answer an agent's permission prompt. They are sent as an ordinary
/// `RunInput`, so the raw log shows exactly what was typed. The adapter owns the keys;
/// `None` when the agent's prompt has no key for that answer.
pub fn permission_input(cli: AgentCli, answer: PermissionAnswer) -> Option<Vec<u8>> {
    match cli {
        AgentCli::Claude => adapters::claude::permission_keys(answer),
        AgentCli::Codex => adapters::codex::permission_keys(answer),
    }
}

/// Exhaustive over `RunPurpose`; the only place a purpose becomes a command.
pub fn plan(purpose: &RunPurpose, env: &PlanEnv) -> Invocation {
    match purpose {
        RunPurpose::Agent(launch) => Invocation::Process(match launch.cli {
            AgentCli::Claude => adapters::claude::launch(launch, env),
            AgentCli::Codex => adapters::codex::launch(launch, env),
        }),
        RunPurpose::Snapshot { .. } => Invocation::InProcess(Effect::Snapshot),
        RunPurpose::Merge(approved) => Invocation::InProcess(Effect::Merge(approved.clone())),
        RunPurpose::Review(spec) => Invocation::Process(adapters::llm::review(spec, env)),
        RunPurpose::Check(req) => match &req.spec {
            CheckSpec::Tla { .. } => Invocation::Process(adapters::tlc::command(req, env)),
            CheckSpec::Lean { .. } => Invocation::Process(adapters::lean::command(req, env)),
            CheckSpec::Device { .. } => adapters::eas::device_check(req, env),
        },
        RunPurpose::Summarize { llm, .. } | RunPurpose::Insight { llm, .. } => Invocation::Process(
            adapters::llm::headless(llm, adapters::llm::ToolAccess::None, Workdir::Home, env),
        ),
        RunPurpose::WriteSkill(draft) => Invocation::InProcess(Effect::WriteSkill(draft.clone())),
        RunPurpose::Import { transcript, cli } => Invocation::InProcess(Effect::Import {
            transcript: transcript.clone(),
            kind: cli.session_kind(),
        }),
    }
}
