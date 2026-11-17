# Review rules

Reviewers (human or AI) read this file from the base branch.

Tag each finding Important, Nit, or Pre-existing. Do not hold pre-existing issues against the change.

Check these when the diff touches the trigger:

| Trigger in the diff | Check |
| --- | --- |
| A new external effect (process, network call, billed session, git write) | It is a Run: intent recorded before the effect, a derived id, and an `OnInterrupt` that does not repeat billed or conversational effects. |
| `LeaseOpened` / `LeaseBound` / `LeaseClosed` or EAS session code | The lease is recorded before the session starts, released on every exit path, and reconciled at startup. `specs/tla/Lease.tla` still models the code. |
| Merge, `Decided`, `ApprovedHead`, or feedback delivery | Merge needs an approval of the current head. Feedback is sent at most once. `specs/tla/ReviewGate.tla` still models the code. |
| Cursor, dedup, or replay code in argus-log / argus-core | Replaying applied frames is a no-op (`proofs/ArgusProofs/Replay.lean`). Property tests cover crash-at-any-byte. |
| A decoder or adapter for agent output | Raw bytes stay stored verbatim. Unknown input decodes to `Unrecognized`, never to nothing. |
| The op list or a view model | Generated TS is regenerated. No domain type leaks onto the wire. |
| `HashMap` / `HashSet` | Not allowed (iteration order leaks into views); use `BTreeMap` / `BTreeSet`. |
| A test that sleeps, retries, or is skipped | Not allowed. Find the cause. |
