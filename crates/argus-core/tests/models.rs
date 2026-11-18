//! Model choices as text, and review panels: one run per model, ids derived from the request.

use argus_core::event::{ReviewLens, ReviewSpec, RunPurpose};
use argus_core::ids::{GitOid, Id128, ModelId};
use argus_core::models::{AgentCli, Effort, ModelChoice, ModelSet};

fn choice(cli: AgentCli, model: &str, effort: Option<Effort>) -> ModelChoice {
    ModelChoice {
        cli,
        model: ModelId(model.into()),
        effort,
    }
}

#[test]
fn a_model_choice_reads_as_cli_model_and_effort() {
    assert_eq!(
        choice(AgentCli::Claude, "opus", None).to_string(),
        "claude:opus"
    );
    assert_eq!(
        choice(AgentCli::Codex, "gpt-5", Some(Effort::High)).to_string(),
        "codex:gpt-5:high"
    );
    assert_eq!(
        choice(AgentCli::Claude, "sonnet", Some(Effort::Max)).to_string(),
        "claude:sonnet:max"
    );
}
