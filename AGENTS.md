# Working on argus

Read [docs/design/RATIONALE.md](docs/design/RATIONALE.md) for why argus is
shaped this way, [docs/architecture.md](docs/architecture.md) for the crates
and the layers of `argus-core`, and [docs/feature-map.md](docs/feature-map.md)
for the names of features and where each one lives. Use those names in commits
and pull requests. Reviewers read [REVIEW.md](REVIEW.md).

The checks hold the rules. `crates/argus-core/tests/architecture.rs` checks
the module layers, the pure modules, and the crate graph. `clippy.toml` and
`[workspace.lints]` ban the APIs that have a wrapper, and
`scripts/check-escape-hatches.sh` reads added lines. When one fails, its
message names the fix. Change the design to pass it; do not silence the check.

## Before you call a change done

- Run `scripts/verify` (it enters `nix develop` itself), then read
  `target/verify/summary.md`. Check each `docs/verification.md` item it lists.
- UI change: `scripts/verify ui`. Op list change: regenerate the client with
  `(cd ui && pnpm gen)` and commit it.
- Spec or proof change: `specs/tla/mutants.txt` configs must keep failing for
  their listed reason, and Lean takes no `sorry` and no new `axiom`.
- Never run a billed test (`ARGUS_ALLOW_BILLED=1`) unless a human asked.

## When a bug is fixed or a review finds something

First ask whether a lint, a type, a test, the architecture test, or a
`docs/verification.md` item can stop it from coming back, and add that.
Write a rule here only when none of them can express it.
