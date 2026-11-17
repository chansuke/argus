//! Which agent CLI and which model. Every LLM-backed run (review, summary, insight, a
//! device-check driver) goes through a headless agent CLI, so argus holds no API keys and
//! needs no HTTP/TLS client, and every call leaves a raw transcript like any other run.
//!
//! A review is asked of a [`ModelSet`], resolved from what the human clicked
//! ([`ReviewerChoice`]) when the request is written. The stored runs therefore say exactly
//! which models looked, even if the presets change later.

use std::collections::BTreeMap;

use crate::ids::ModelId;

/// The agent CLIs argus can launch.
///
/// Adding one (OpenCode, Gemini, an ACP agent) is this variant plus what the compiler then
/// demands, and nothing else:
/// 1. `adapters/<agent>.rs`: `launch` (interactive, under a PTY), a decoder per byte
///    format it writes, and a `headless` arm in `adapters::llm` if it can review.
/// 2. `SourceKind`: one variant per format, placed in a family by the exhaustive
///    `SourceKind::family()`, so every view that reads that family sees it.
/// 3. The exhaustive matches in `plan::plan` and `AgentCli::session_kind`.
/// 4. `argus_api::vm::Agent` plus the host's two exhaustive mappings (wire <-> core).
///
/// No variant exists before its adapter does, so no API input can reach an unwritten one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AgentCli {
    Claude,
    Codex,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Effort {
    Low,
    Medium,
    High,
    Max,
}

/// `claude:opus`, `codex:gpt-5:high`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ModelChoice {
    pub cli: AgentCli,
    pub model: ModelId,
    pub effort: Option<Effort>,
}

/// Non-empty, sorted, deduplicated. A review asked of zero models is unrepresentable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModelSet {
    first: ModelChoice,
    rest: Vec<ModelChoice>,
}

impl ModelSet {
    pub fn new(models: impl IntoIterator<Item = ModelChoice>) -> Option<Self> {
        let mut all: Vec<ModelChoice> = models.into_iter().collect();
        all.sort();
        all.dedup();
        let mut it = all.into_iter();
        Some(ModelSet {
            first: it.next()?,
            rest: it.collect(),
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = &ModelChoice> {
        std::iter::once(&self.first).chain(&self.rest)
    }
}

/// Kebab-case preset name from config.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PresetName(String);

impl PresetName {
    pub fn parse(s: &str) -> Option<Self> {
        let ok = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        ok.then(|| PresetName(s.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the reviewer clicked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReviewerChoice {
    /// The default preset: AI review is one click.
    Default,
    Preset(PresetName),
    Models(ModelSet),
}

/// Named model sets from config, e.g. `quick = ["claude:sonnet"]`,
/// `panel = ["claude:opus", "codex:gpt-5:high"]`. A panel is N review runs of one tree;
/// agreement between them is derived at read time (`views::packet`).
#[derive(Clone, Debug)]
pub struct ModelPresets {
    presets: BTreeMap<PresetName, ModelSet>,
    default: PresetName,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownPreset(pub PresetName);

impl ModelPresets {
    /// `None` when `default` is not one of `presets`.
    pub fn new(presets: BTreeMap<PresetName, ModelSet>, default: PresetName) -> Option<Self> {
        presets
            .contains_key(&default)
            .then_some(ModelPresets { presets, default })
    }
    pub fn resolve(&self, choice: &ReviewerChoice) -> Result<ModelSet, UnknownPreset> {
        let name = match choice {
            ReviewerChoice::Models(set) => return Ok(set.clone()),
            ReviewerChoice::Default => &self.default,
            ReviewerChoice::Preset(name) => name,
        };
        self.presets
            .get(name)
            .cloned()
            .ok_or_else(|| UnknownPreset(name.clone()))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&PresetName, &ModelSet)> {
        self.presets.iter()
    }
    pub fn default_name(&self) -> &PresetName {
        &self.default
    }
}
