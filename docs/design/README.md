# Design

- [RATIONALE.md](RATIONALE.md): the architecture (event-sourced per-actor logs, views as folds, an attention-ranked review gate), how it was chosen, and the open questions.
- [grounding.md](grounding.md): the product brief, reference products, and environment facts the design was built against.
- UI design (Claude Design canvas, private): https://claude.ai/artifact/1S8tKeRwSbi21zTn5hfTMN. Desktop: Glance, Review, Timeline, Raw log, Assets. Mobile: Needs you, Review.

## Decisions

| Date | Decision |
| --- | --- |
| 2026-11-17 | Desktop UI is Tauri v2 with React + TypeScript. |
| 2026-11-17 | The mobile app is Expo. EAS Simulators verify the mobile app and also produce device evidence for agent runs on Expo projects. |
| 2026-11-17 | A paired phone may approve, request changes, merge, and answer permission prompts. |
| 2026-11-17 | The dev environment is a Nix flake. Verification gates and CI come before feature work. |
| 2026-11-18 | A damaged log is cut at its first bad frame, but the cut-off bytes are first copied to `<actor>.alog.torn-<offset>`, so the host still starts and nothing is lost. |
| 2026-11-18 | Stay on Tauri; revisit react-native-gpui when it leaves alpha (see RATIONALE open questions). |
