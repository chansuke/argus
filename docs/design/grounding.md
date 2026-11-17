# Grounding: argus (self-built ADE)

Repo: /home/chansuke/code/myprojects/argus — empty git repo, no commits. Greenfield; no codebase to integrate.
Name "argus" (hundred-eyed watcher) fits the product: the human sees everything the agents do.

## Product brief (from the user, translated)
An Agent Development Environment inspired by Orca and Devin Desktop. Six differentiators:
1. Review — maximize the human's review ability. AI review must be easy to invoke, including model selection.
2. Formal verification — integrate Lean and TLA+ checking.
3. Timeline — follow what happened when, chronologically (inspired by deepseek-harness "timeline" plugin, below).
4. Progress visualization — one glance tells how far along work is and what is happening right now.
5. Raw logs — see what the agent (LLM) actually thought and called, without summaries hiding anything.
6. Conversation logs as assets — analysis, time-period slicing, and turning improvements/ideas found in a session into skills.

## Reference products (confirmed from vendor pages / directories; details partly marketing)
- Orca (stablyai/orca, MIT): desktop app running many terminal coding agents in parallel (Claude Code, Codex, OpenCode, Gemini, ...), one git worktree per task; fan one prompt to N agents, compare, merge the winner; SSH worktrees; GitHub/Linear integration; an Orca CLI agents can call. "If it runs in a terminal, it runs in Orca."
- Devin Desktop (Cognition, ex-Windsurf, renamed 2026-06): editor + Agent Command Center for local and cloud agents; Devin Local agent; "Quick Review" = secondary agent reviews the diff and shows feedback inline in the editor; ACP (Agent Client Protocol)-compatible agent support.
- deepseek-harness timeline plugin (@deepseek-ai/dsh-timeline): agent maintains a task tree via tools (timeline_tree/tasks/pull/read/reply). Tasks: title, description, results, blockers, priority, weight, revision history, states completed/in-progress/pending/blocked. Progress = weight-share among siblings, subtask progress is a share of parent share (splitting does not change parent total). Timed reports at levels (15m/1h/4h), each level rolls up the lower level. Discussions attach to task/report. Session log is source of truth; a session projection rebuilds state on reopen. `.timeline.json` export. Reminder if agent goes 24 iterations without a task update. Known flaw: O(N^2) replay from duplicate-ID scanning on each report event.

## Environment facts (confirmed locally)
- User writes Rust heavily (edition 2024, std-only workspaces, strict clippy). They have their own TLA+ model checker in Rust: ~/code/myprojects/tlars (pure Rust reimplementation of TLC, crates tlars-check, tlars-sany, ...; aims to be byte-identical to `java tlc2.TLC -tool` output and exit codes). Use it as a TLA+ backend option alongside java TLC. Lean backend = `lake build` / `lean --json` diagnostics.
- OS/env is Nix flake managed (Linux). Shell fish, editor Neovim (so the ADE need not be a full editor; diff/review UI matters more than editing).
- Claude Code transcripts live at ~/.claude/projects/<slug>/<session>.jsonl, one JSON object per line. Observed record `type`s: user, assistant, attachment, api-request, api-request-blob (hash-addressed full request message), permission-mode, mode, last-prompt, ai-title, pr-link, bridge-session. Assistant `message.content[]` block types: text, thinking, tool_use. User content: string or [tool_result]/[text]. Records carry uuid, parentUuid (a tree, not a list), timestamp, sessionId, isSidechain, cwd, gitBranch, requestId. Subagent transcripts under <session>/subagents/. Format is undocumented and changes between versions → must be parsed behind an adapter, raw bytes preserved.
- Other agents (Codex, OpenCode, ...) have their own formats; ACP is an emerging common protocol.

## Defaults chosen by the orchestrator (runners may challenge with reasons)
- Core in Rust (Cargo workspace). UI shell is your call (Tauri, local web server + browser, TUI) but keep the domain core UI-agnostic.
- Local-first, single user, data on disk under the repo or ~/.local/share/argus. No cloud backend in v1.
- Agents are external processes (Claude Code, Codex, ...) in git worktrees, as in Orca. argus does not implement its own LLM agent loop, but it does call LLM APIs for AI review / analysis / skill extraction with user-chosen models.
- Skills output target: Claude Code skill format (a directory with SKILL.md + frontmatter) under ~/dotfiles-managed or project .claude/skills/.

## User decision (2026-11-17)
- UI shell is Tauri (v2). Domain core stays UI-agnostic; `argus-app` is a thin Tauri layer (commands/events only, no business logic).
- Desktop frontend: React + TypeScript in Tauri v2.
- Mobile app: Expo (React Native + TS). Desktop core exposes a network API for it; one command/query/subscription list in the core derives both Tauri IPC and HTTP/WS transports and the TS types.
- EAS Simulators (https://docs.expo.dev/eas/simulators/introduction/): remote iOS Simulator / Android Emulator on Expo infra; sessions via EAS CLI / REST / link; controlled via agent-device, Argent, Appium; produces screenshots/recordings/logs; no physical devices; must stop sessions explicitly (billing). Used to verify the Expo app and as an evidence producer for agent runs on Expo projects. Pricing/limits not stated on the page (未確認).
- Visual design via Claude Design (Artifact design type), done after the core sketch.
