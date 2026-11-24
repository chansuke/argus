# Verification checklist

Each item below has a trigger you can read from a diff and a check that a test
or a gate step does not fully cover. `scripts/verify` lists the items a change
triggers in `target/verify/summary.md`. Check only those.

```sh
scripts/verification-checklist.sh             # changes since main, uncommitted ones included
scripts/verification-checklist.sh A~1 A       # the changes of commit A
```

The script reads the `Paths` and `Added lines` of each item. A path is a shell
pattern in which `*` also matches `/`. An added-lines pattern is an extended
regular expression matched against the lines the diff adds.

## Adding and removing items

Add an item when a reviewer had to catch the same mistake twice and a diff
shows when it applies. Remove an item once a lint, a type, a test, or a gate
step catches the mistake on its own. The review rules in REVIEW.md that a
check now enforces (HashMap, sleeping tests) are not repeated here.

## Items

### A new external effect is a Run

- Paths: `crates/argus-host/src/runner.rs`, `crates/argus-host/src/lease.rs`, `crates/argus-host/src/git.rs`, `crates/argus-host/src/proc.rs`, `crates/argus-core/src/plan.rs`, `crates/argus-core/src/command.rs`, `crates/argus-core/src/adapters/*`
- Added lines: `proc::command\(`, `Command::new\(`, `RunPurpose::[A-Z]`
- Check: the intent is recorded before the effect, the run id is derived, and
  `OnInterrupt` does not repeat a billed or conversational effect. A test
  should crash the host between intent and effect and replay.

### Leases are recorded and released

- Paths: `crates/argus-host/src/lease.rs`, `crates/argus-core/src/adapters/eas.rs`, `specs/tla/Lease*`
- Added lines: `Lease(Opened|Bound|Closed)`
- Check: the lease is recorded before the session starts, released on every
  exit path, and reconciled at startup. `specs/tla/Lease.tla` still models the
  code (`tools/gate tla`).

### Merge needs an approval of the current head

- Paths: `crates/argus-core/src/views/review.rs`, `crates/argus-host/src/git.rs`, `specs/tla/ReviewGate*`
- Added lines: `Decided|ApprovedHead|RunPurpose::Merge|FindingPromoted`
- Check: merge takes only an `ApprovedHead` of the current head, and feedback
  is sent at most once. `specs/tla/ReviewGate.tla` still models the code.

### Replaying applied frames is a no-op

- Paths: `crates/argus-log/src/*`, `crates/argus-core/src/ledger.rs`, `proofs/*`
- Added lines: `cursor|dedup|replay`
- Check: `proofs/ArgusProofs/Replay.lean` still states the code's rule, and
  the crash-at-any-byte property tests cover the changed path.

### Foreign bytes stay verbatim

- Paths: `crates/argus-core/src/adapters/*`, `crates/argus-core/src/source.rs`
- Check: raw bytes stay stored verbatim, unknown input decodes to
  `Unrecognized`, never to nothing, and the adapter's `VERSION` is bumped when
  its output changes.

### No domain type on the wire

- Paths: `crates/argus-api/src/ops.rs`, `crates/argus-api/src/vm.rs`, `crates/argus-host/src/handler.rs`
- Check: view models are presentation shapes; the host maps core enums both
  ways with exhaustive matches. The generated TS is regenerated (`tools/gate
  tsgen` fails otherwise).

### The phone bundle does not load Tauri

- Paths: `ui/*`
- Check: nothing the mobile app imports reaches `@tauri-apps/api` or
  `@argus/client/tauri`.
