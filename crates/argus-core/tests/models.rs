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

#[test]
fn a_model_choice_parses_back_from_its_text() {
    for c in [
        choice(AgentCli::Claude, "opus", None),
        choice(AgentCli::Codex, "gpt-5", Some(Effort::High)),
        choice(AgentCli::Claude, "sonnet", Some(Effort::Low)),
    ] {
        assert_eq!(ModelChoice::parse(&c.to_string()), Some(c));
    }
    // A model id may itself contain colons; only a known effort is split off the end.
    assert_eq!(
        ModelChoice::parse("claude:us.anthropic.claude-v1:0"),
        Some(choice(AgentCli::Claude, "us.anthropic.claude-v1:0", None))
    );
    for bad in [
        "",
        "claude",
        "claude:",
        "gemini:pro",
        "Claude:opus",
        "claude::high",
    ] {
        assert_eq!(ModelChoice::parse(bad), None, "{bad:?}");
    }
}

fn oid(c: char) -> GitOid {
    GitOid::parse(&c.to_string().repeat(40)).unwrap()
}

fn panel_of(request: u128, focus: Option<&str>) -> Vec<(argus_core::ids::RunId, ReviewSpec)> {
    let models = ModelSet::new([
        choice(AgentCli::Codex, "gpt-5", Some(Effort::High)),
        choice(AgentCli::Claude, "opus", None),
    ])
    .unwrap();
    ReviewSpec::panel(
        Id128(request),
        &oid('1'),
        &oid('b'),
        ReviewLens::Security,
        focus,
        &models,
    )
    .into_iter()
    .map(|(run, purpose)| match purpose {
        RunPurpose::Review(spec) => (run, spec),
        other => panic!("{other:?}"),
    })
    .collect()
}

#[test]
fn a_panel_is_one_review_run_per_model_with_derived_ids() {
    let runs = panel_of(1, None);
    assert_eq!(runs.len(), 2);
    assert_ne!(runs[0].0, runs[1].0);
    // Asking again (a retry, or the policy deciding twice) gives the same runs.
    assert_eq!(
        panel_of(1, None)
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>(),
        runs.iter().map(|(r, _)| *r).collect::<Vec<_>>()
    );
    assert_ne!(panel_of(2, None)[0].0, runs[0].0);
    let models: Vec<String> = runs.iter().map(|(_, s)| s.llm.model.to_string()).collect();
    assert_eq!(models, vec!["claude:opus", "codex:gpt-5:high"]);
    for (_, spec) in &runs {
        assert_eq!(
            (&spec.tree, &spec.base, spec.lens),
            (&oid('1'), &oid('b'), ReviewLens::Security)
        );
    }
    // Every model is asked the same question.
    assert_eq!(runs[0].1.llm.prompt, runs[1].1.llm.prompt);
}

#[test]
fn the_review_prompt_names_the_base_the_lens_and_the_output_contract() {
    let runs = panel_of(1, Some("the lease recovery path"));
    let prompt = &runs[0].1.llm.prompt;
    assert!(
        prompt.contains(&format!("git diff {}", oid('b').as_str())),
        "{prompt}"
    );
    assert!(prompt.contains("security"), "{prompt}");
    assert!(prompt.contains("the lease recovery path"), "{prompt}");
    assert!(prompt.contains("```json"), "{prompt}");
    for field in [
        "\"path\"",
        "\"start\"",
        "\"end\"",
        "\"severity\"",
        "\"title\"",
        "\"body\"",
    ] {
        assert!(prompt.contains(field), "{field} missing from {prompt}");
    }
    assert!(prompt.contains("high, medium, low, nit"), "{prompt}");
    assert!(!panel_of(1, None)[0].1.llm.prompt.contains("lease recovery"));
}

#[test]
fn preset_names_are_lowercase_words_joined_by_hyphens() {
    use argus_core::models::{ModelPresets, PresetName};
    use std::collections::BTreeMap;
    assert_eq!(PresetName::parse("panel-2").unwrap().as_str(), "panel-2");
    for bad in ["", "Panel", "panel 2", "panel_2", "pánel"] {
        assert!(PresetName::parse(bad).is_none(), "{bad:?} accepted");
    }
    let set = ModelSet::new([choice(AgentCli::Claude, "opus", None)]).unwrap();
    let quick = PresetName::parse("quick").unwrap();
    let presets = ModelPresets::new(
        BTreeMap::from([(quick.clone(), set.clone())]),
        quick.clone(),
    )
    .unwrap();
    let listed: Vec<_> = presets
        .iter()
        .map(|(name, s)| (name.as_str(), s.clone()))
        .collect();
    assert_eq!(listed, vec![("quick", set)]);
}
