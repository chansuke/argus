---
name: verify
description: Run argus's verification gate (fmt, clippy, tests, generated TS, typecheck, typos, actionlint, TLA+ model checking with mutants, Lean proofs) before committing or reporting work as done. Use after any change to crates/, ui/, specs/, or proofs/.
---

# Verify

Run the gate in the dev shell:

```bash
nix develop -c tools/gate            # every offline step
nix develop -c tools/gate clippy test  # only some steps
nix develop -c tools/gate --list
```

The same script runs in CI (`.github/workflows/ci.yaml`) and in the Stop hook, so a local pass means a CI pass apart from network-only steps.

Rules:

- Fix the cause of a failing step. Do not weaken an assertion, a TLA+ invariant, or a mutant's expected exit code to make it pass.
- `specs/tla/mutants.txt` lists configs that model designs argus rejected. They must keep failing. If you change a spec, run `tools/gate tla` and check that each mutant still fails for the listed reason.
- A Lean `sorry` fails the build. Do not add `axiom`.
- When the op list in `crates/argus-api/src/ops.rs` changes, regenerate the client with `(cd ui && pnpm gen)` and commit the result.
- If a step still fails after three rounds of fixes, stop and report the failing output to the human.
- Do not retry a flaky test until it passes. Find the cause, or delete the test and write down why.
- Edits to the gate itself (tools/, .github/, flake.nix, specs/tla/*.cfg, mutants.txt, proofs/lakefile.toml) are flagged on pull requests for human review. Keep them in their own commit.
