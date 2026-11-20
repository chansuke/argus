# argus: an event-sourced ADE with an attention-ranked review gate

## Problem

argus is a self-built Agent Development Environment. External coding agents (Claude Code, Codex, ...) run in git worktrees. The human must review, verify, and follow them more easily than in Orca or Devin Desktop. It covers six features: review with AI (with model choice), formal verification (Lean, TLA+), a timeline, progress at a glance, unfiltered raw logs, and conversation logs as analyzable assets that become skills. Review comes first: the scarce resource is the human's attention, so the review screen must rank what to look at, link every hunk to the agent's own words, and turn the human's verdict into the agent's next prompt without the human sequencing calls.

Five constraints make the shape non-obvious:

- **The richest input is a foreign, undocumented, changing format.** Claude Code JSONL is a record tree (`uuid`/`parentUuid`) with record types that come and go. It must be parsed behind an adapter, and the raw bytes must be kept. Agents may also rewrite or truncate their own logs.
- **Many writers act at once.** Agents, reviewers, checkers, the human on desktop and phone, and automatic policy all write, and the host can crash mid-work.
- **Some work costs money while it runs.** EAS Simulators sessions bill until they are stopped explicitly. Every AI review run costs money too.
- **A verdict must mean "this exact code".** Agents keep editing while the human reviews, so approval, merge, and feedback must bind to a snapshot, not to "the current diff".
- **There are two frontends.** A Tauri v2 + React desktop and an Expo phone app must not get two diverging APIs.

The dsh-timeline plugin shows the right instinct (the session log is the source of truth and state is a projection) and a known flaw: O(N²) replay from duplicate-id scans.

Constraints honored: Rust 2024, std-only workspace (`cargo check --offline --workspace --all-targets` and `cargo clippy` pass with zero warnings; Tauri is excluded from members), `HashMap`/`HashSet` disallowed by `clippy.toml`, local-first single user, agents are external processes, and LLM calls use a model chosen by the user.

## Usage (caller's view)

Quickstart (human, fish):

```fish
argus run --agent claude:opus --agent codex:gpt-5 "Add retry to the uploader"   # fan-out: 2 jobs, 1 group
argus status                                      # the attention queue, then one card per job
argus review j-7f3a --preset panel --lens security   # one review run per model in the preset
argus packet j-7f3a                               # headline, then hunks by tier, each with provenance
argus raw j-7f3a --fact human:12.0                # verbatim bytes behind any item on screen
argus promote j-7f3a <finding>                    # an AI finding becomes a draft comment
argus feedback j-7f3a                             # exactly what RequestChanges will send
argus decide j-7f3a request-changes --note "keep the old retry budget"
argus decide j-7f3a approve && argus merge j-7f3a
argus check j-7f3a tla specs/Upload.tla --engine tlars    # same target with --engine tlc to compare
argus check j-7f3a device apps/mobile --platform ios --app fingerprint:ab12
argus stats --period last-7d --by model
argus insights --period 2026-11 && argus skill accept <candidate> --to ~/dotfiles/.../skills
```

Inside an agent (env `ARGUS_RUN`, `ARGUS_SOCKET`, `ARGUS_TOKEN` set at launch). These calls are optional; argus works without them:

```fish
argus task add "Parser" --weight 2; argus task block <id> "needs fixture"; argus task done <id> --result "..."
argus check --self tla specs/Upload.tla          # agents may request checks/reviews of their own work
```

Host code (Rust) uses one write path and one read path per ledger:

```rust
let mut job = store.open_job(job_id)?;                       // takes the job's locks (single host)
for (run, purpose) in ReviewSpec::panel(request, &head, &base, ReviewLens::Security, None, &models) {
    job.submit(Author::Human, Event::RunRequested { run, purpose })?;  // retry -> Submitted::Duplicate
}
let req = job.views().review().diff_request();               // what to diff; git runs off the sequencer
let packet = job.views().review_packet(&head_diff, &cfg)?;   // Err(HeadMoved) -> diff again
job.submit(Author::Human, Event::Decided { decision, tree: Some(head), verdict: Decision::Approve, note })?;
// ^ refused with Rejected::NotHead if a newer snapshot landed: a verdict always names exact code
let g = job.views().glance(now);                             // state, share, attention (most urgent first)
```

Frontends (React desktop and Expo phone) use the same generated client:

```ts
import { createClient, requestId } from "@argus/client";
import { tauriTransport } from "@argus/client/tauri";   // phone: websocketTransport(url, pairingToken)
const api = createClient(tauriTransport());
api.overview({}, (o) => renderQueue(o.queue));              // the phone's home screen: what needs me
api.review({ job }, (screen) => render(screen.headline, screen.agenda));   // pushed when inputs change
await api.request_review({ request_id: requestId(), job, reviewers: { kind: "Preset", name: "panel" }, lens: "Security", focus: null });
await api.promote_finding({ request_id: requestId(), job, finding: item.findings[0].id });
const { text } = await api.preview_feedback({ job, note: "" });
await api.decide({ request_id: requestId(), job, tree: screen.head!.tree, verdict: "RequestChanges", note: "" });
await api.answer_permission({ request_id: requestId(), job, run, answer: "Allow" });  // one tap on the phone
```

## Shape

**Crates.** `argus-log` (bytes) → `argus-json` → `argus-core` (domain, std-only, no UI) → `argus-api` (the one surface) → `argus-host` (process shell + transports) → `argus-cli`. `argus-app` is Tauri and excluded from members. `ui/` is a pnpm workspace with `@argus/client` (generated) plus `apps/desktop` and `apps/mobile`.

**Data, first.**
- A **ledger** is a directory of append-only **per-actor logs**, one per job plus one store-wide meta ledger. Actors are `human`, `host`, `policy`, `agent-<run>` (self-report), and `src-<source>`, one per raw byte stream: transcript file, stdout, PTY, hook payloads, `eas simulator:events`.
- Each log has exactly one writer, enforced by an OS lock in `LogWriter::open`. A frame's identity is `(actor, seq)`, where `seq` is its dense position in the file. No id is ever minted, so none can collide. Outside `argus-log` a `Seq` comes only from the writer, a reader, or `EventId::parse` of a stored reference.
- **Authored events** (`Event`) record what argus itself originates. **Raw frames** record foreign bytes verbatim, one meaningful unit each (a JSONL line, a TLC message block, a PTY chunk), or a `Discontinuity` marker when the agent rewrote its file.
- The rule deciding which is which: *non-deterministic things are recorded* (human decisions, process outcomes, LLM answers, remote ids, timing); *deterministic things are derived*. Two cached readings of immutable git trees are recorded with each `Snapshot` (hunk headers vs the previous tree, blob ids and hunk anchors vs base) so that projections never call git; they cannot disagree with git.
- Whole immutable files (screenshots, recordings) go in a content-addressed blob store. A blob is written before the event that names it.

**Flow.**
- `submit` checks authorship and ledger scope (`Event::check`, exhaustive), drops re-submissions by identity (`dedup_key`, an ordered index), runs the view-dependent `ViewSet::admit` (head-only decisions, current approval) on the sequencer, syncs dirty raw logs, appends, fsyncs, and applies. Check and append are atomic because one sequencer owns the ledger.
- `observe` appends raw frames and decodes them through `SourceKind::decode` into a non-empty `Decoded` list (unreadable means `Unrecognized`, never nothing). Every observation that passes the record rule reaches the views as a `Fact`.
- **The record rule, in one place (`ledger::Applied`):** an agent record key (Claude `uuid`) is applied once per ledger. A later frame with the same key (a `--resume` that copies history, a file re-read after a `Discontinuity`) is stored and shown in the raw view as a repeat, but never reaches a projection. Live apply, replay, and checkpoints carry this state, so every view counts each record once.
- Views are pure folds registered in one `view_set!` list. Each declares the **source families** it reads (`Projection::READS`), and `SourceKind::family()` is one exhaustive match, so a new agent's formats cannot be silently invisible to some view. Replay is a k-way merge by stamp from one sequencer, so **replay order equals live order**; `Cursor::admit` makes re-delivery a no-op; total replay is O(N log N).
- Read-time compositions (`glance`, `review_packet`, `preview_feedback`) are pure functions over several views. They are computed when asked and never stored.

**Load-bearing decisions.**
1. **Raw bytes are the stored fact; parsed agent events are derived on every replay.** Decoders are stateless per frame and versioned (the version is part of every checkpoint key), so fixing an adapter repairs all history. Transcripts are copied rather than referenced. When an agent rewrites its transcript, the tailer appends a `Discontinuity` frame and continues from the new content; stored bytes are never edited. The one exception to verbatim capture is credentials: `eas simulator:start --json` stdout contains `remoteConfig`, so it is parsed in memory and never captured.
2. **Every external effect is a Run.** `RunRequested` → `RunStarted{attempt}` → raw output sources → `RunFinished{attempt, outcome}`. `RunPurpose` is the closed list (Agent, Snapshot, Review, Check, Merge, Summarize, Insight, WriteSkill, Import); `plan()`, `on_interrupt()`, `ledger_kind()`, and `Event::check` are exhaustive over it. Crash recovery is `Runs::due(live)`: abandon interrupted attempts, retry those whose purpose allows it (never agents, never billed device sessions), release open leases. Policy decisions use ids derived from their trigger, so deciding twice is a dedup no-op. (per make-operations-idempotent)
3. **Billed resources are leases.** `LeaseOpened` (the intent) is durable before `eas simulator:start` runs, then `LeaseBound{remote_id}`, then `LeaseClosed`, with release on every exit path and at startup. Provider-side `--max-duration-minutes` and `--max-idle-time-minutes` cap the cost even if argus never comes back.
4. **Progress needs no self-report.** The observed plan comes from the agent's native plan tool (`TodoWrite`, `TaskCreate`/`TaskUpdate`); "now" comes from the tail of its stream plus hook permission waits. Self-report through `argus task` / MCP only adds weights, blockers, and results. Share uses dsh semantics.
5. **The review packet is a read-time view over the snapshot chain.** `JobViews::review_packet(head_diff)` composes a five-second `Headline`, then one `AgendaItem` per head hunk sorted by `Tier` (Blocking > Suspicious > Normal > Collapsed, the derived `Ord`), then what no hunk holds. One `tier()` function holds the rules: located check failures, high findings that at least two models agree on, and sent threads whose lines are unchanged at head (unaddressed) are Blocking; single-model high findings, sensitive paths, and **Unexplained** hunks are Suspicious; a hunk whose anchor appeared in a decided snapshot is Collapsed unless something new is attached, so round two is an interdiff for free. Machine evidence from an older snapshot is shown only as `CarriedForward` when every file it can depend on is byte-identical at head; human threads re-anchor by lines through the chain. The hunk text comes from `git diff` in the host, on the API thread and memoized by `(base, tree)`, so the sequencer is never held while git runs. (per single-source-of-truth: nothing about review is stored that can be derived)
6. **Every hunk links to the agent's own words.** Adapters read each tool call's input into `FileTouch`es (only they know `Edit`, `Write`, `MultiEdit`, `apply_patch`, `Bash`). The transcript view indexes them as `EditTrace`s in stamp order. A **revision window** is the edits between two consecutive snapshot cuts. `provenance()` matches a hunk's added-line hashes against inserted-line hashes: `Direct{facts}`, `ViaShell{facts}`, or `Unexplained`. Both hashes and the diff parser live in `diff.rs`, so the host and the adapters cannot hash differently.
7. **The verdict closes the loop, and only the human decides.** Threads are drafts until `Decided{RequestChanges}` sends them. AI findings reach the agent only through `FindingPromoted`. One pure `render_feedback` serves both the `preview_feedback` query and delivery, so the preview is byte-for-byte what is sent. Delivery is a policy-requested `RunPurpose::Agent{resume: Some(prev)}` whose `RunId` is derived from the `DecisionId`; agent runs are never retried after a crash, so delivery is at-most-once and a failed one surfaces as `Attention::DeliveryFailed`. `ApprovedHead` is minted only by `ReviewBoard::approved_head()` (the latest decision on the head tree is Approve), `RunPurpose::Merge` takes nothing else, and `admit` re-checks it at append. A new snapshot changes the head, which revokes approval by construction. Merge uses `git merge-tree --write-tree`, `commit-tree`, and a compare-and-swap `update-ref`, so no checkout is touched.
8. **One API surface, many transports.** `argus-api/src/ops.rs` is the single list of `query` / `command` / `subscribe` ops with `[agent]` marks. A macro generates the host's `Handler`, the one `dispatch`, a typed Rust `Client`, and the TypeScript (`gen/api.ts`, type-checked with `tsc --strict`). Every closed set on the wire is a `vm_tags!` enum, emitted as a string-literal union (`"Claude" | "Codex"`); the host maps wire and core enums with exhaustive matches both ways. Transports (Tauri IPC with 3 generic commands, WebSocket for the phone, the Unix socket for the CLI and agents) only move frames and establish the `Caller`. Commands carry client-minted `request_id`s, so retries over a flaky phone link are safe.
9. **Subscriptions recompute only what changed.** Each view has a version, bumped when its `apply` reports a change. Views are reached only through generated accessors that record the read, so a subscription depends on exactly the views its code read (no dependency list to keep in sync). After a batch, a ledger marks dirty only subscriptions whose stamp for that ledger moved; dirty ones are re-evaluated at most every ~100 ms and pushed only when their JSON differs.

**Secrets and the phone.**
- Snapshots stage tracked changes plus untracked files that pass git's ignore rules and the argus deny list (`.env*`, `*.pem`, `*.key`, `id_*`, `.netrc`, ...; config can add, not remove). Withheld files are listed in the snapshot and on the review screen, so nothing silently disappears, and a secret never enters a tree, a ref, or a reviewer's checkout.
- Retention: only terminal (`src-*` PTY) logs, which no projection reads, may be pruned, as whole files, `terminal_days` (default 30) after their run finished, after a `SourcePruned` event.
- The phone listener binds only to loopback or a tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48, per https://tailscale.com/kb/1015/100.x-addresses); anything else is refused at config load. Every connection needs a pairing token. A paired phone is the human, with the same ops as the desktop: it may approve, request changes, merge (through the same `ApprovedHead` check), and answer permission prompts with one tap (`answer_permission`, sent as a normal `RunInput`). This is the user's decision of 2026-11-17.

**Invariants in types.** `ActorKey` is filename-safe. `Seq` is never minted outside `argus-log`. `LogWriter` is not `Clone` and holds the lock. `Decoded` and `ModelSet` are non-empty. `ApprovedHead` cannot be built outside `argus-core`. `CheckSpec` makes engine/target mismatches unrepresentable; `CheckTarget` is engine-independent (TLC vs tlars `disagreements`) and owns `depends_on`, the one rule for "re-run on this change" and "still holds at head". `LineRange` guarantees `start <= end`. `RepoPath` cannot escape the repo. `PhoneListen` cannot hold a public address. Adapters are private (`mod adapters`); views see only `Observation`, the host only `CommandSpec`, and the review prompt is built only inside `ReviewSpec::panel`.

**Validation lives at the boundaries.** `ApiHandler` turns view models into ids, paths, trees, and core enums. `Ledger::submit` checks authorship, scope, and view-dependent admission. Decoders handle foreign bytes. Inside, types are trusted.

**Two clocks.** `Stamp` is append order and orders replay. `Fact.at` is domain time (the record's own timestamp); timeline and analytics key on it. Analytics use hourly buckets, so any whole-hour period is exact.

**Interface depth.** `Ledger` exposes `open/submit/open_source/observe/views/raw(_page)/prune_source/flush` and hides framing, locking, torn-tail repair, stamps, validation, admission, both dedups, fsync policy, decoding, and fan-out. `JobViews` exposes views plus three compositions (`glance`, `review_packet`, `preview_feedback`) and hides ranking, clustering, carry-forward, provenance, and re-anchoring. `Host` exposes `start/call/subscribe/unsubscribe/blob_path` and hides processes, PTYs, leases, git, tailers, policy, delivery, recovery, and dependency tracking. Frontends see one generated `ArgusApi`; the review loop is single calls (`decide`, `merge`), never a sequence the client coordinates.

**Deliberately not done.** No own agent loop. No editor. No HTTP/TLS client and no API keys: all LLM calls go through headless agent CLIs, which also leaves a raw transcript of every review. No automatic forwarding of AI findings to agents. No cloud. No cross-host merge yet, although stamps are HLC-shaped. Agents other than Claude Code and Codex do not exist as variants until their adapters do; `AgentCli`'s doc comment is the one checklist for adding one (OpenCode, Gemini, ACP).

## Synthesis decision

**Base: candidate-1** (event-sourced per-actor logs, raw bytes as the stored truth, versioned decoders, a Run for every effect, leases, one ops list generating Handler/dispatch/Rust client/MCP/TS). The cross-judge scored the two candidates 23 to 23 and recommended candidate-1 as the base with candidate-2's review layer grafted on. Candidate-1 was strongest on storage, crash safety, the raw/parsed boundary, API generation, and store-wide import of existing transcripts. It was weakest on review: no ranking, no hunk-to-transcript link, no approve/request-changes/merge. The orchestrator first leaned toward candidate-2 for its review focus, then agreed with the judge: storage, crash ordering, the raw/parsed boundary, and cross-repo transcript import are hard to retrofit, while candidate-2's review logic is pure functions that port cleanly.

**Grafted from candidate-2**, rewritten into the base's model (facts, views, runs) rather than pasted:
1. Review packet (`argus-change/src/review.rs`): `Tier`, `Signal`, `Headline`, `AgendaItem`, `Applicability{Current, CarriedForward}`, `seen_anchors` → `views/packet.rs`, `JobViews::review_packet`, `vm::ReviewScreen`. It is built from the snapshot chain; snapshots record blob ids and hunk anchors, so carry-forward and "seen" need no git. Changed: a seen hunk with a new finding or failure no longer collapses.
2. Hunk provenance (`argus-change/src/diff.rs`, `argus-transcript` `EditTrace`): `Provenance{Direct, ViaShell, Unexplained}` from inserted-line hashes. Adapters emit `FileTouch` on `ToolCall`, the transcript view indexes `EditTrace`, and a revision window is the edits between two snapshot cuts. Unexplained hunks become `Signal::Unexplained` (Suspicious).
3. Verdict and feedback loop (`desk.rs`, `model.rs`): `Event::Decided` (head-only), drafts until RequestChanges, `promote_finding`, one `render_feedback` for preview and delivery, `ApprovedHead`. Changed: delivery is a policy run with an id derived from the decision. This fixes candidate-2's double send, where the feedback was sent before the `Delivery` record was written.
4. Model presets (`argus-llm`): non-empty `ModelSet`, `ReviewerChoice`, named presets. A panel is N review runs; findings are clustered by overlapping location and `agreeing_models` feeds `Tier`. Kept from the base: LLM calls go through agent CLIs, and malformed output stays `ReviewOutput::Unstructured`.
5. Attention ordering (`progress.rs`): `views/glance.rs` `Attention` plus `queue()`, which is `Overview.queue` and the phone's home screen. Changed: every variant carries `since`. Candidate-2's `Stalled{idle_ms}` would have sorted the shortest stall first.
6. Discontinuity marker (`argus-raw` `mirror_from`): `RawFrame::Discontinuity` in the base's source log; the tailer never stops, and re-read records are dropped by the record rule.

**Base fixes from the red-flag screen.** One exhaustive `SourceKind::family()` replaces five hand-synced `wants` matches (Codex split into `CodexRollout`/`CodexExec`). Closed sets on the wire are typed enums with string-literal TS unions (agent, effort, lens, engine, platform, verdict, group-by, severity, tier, bucket, raw format, item kind), and the anchor is a typed `AnchorInput`. `Seq::from_persisted` is crate-private behind `EventId::parse`. The `review_prompt` re-export is gone. OpenCode/Gemini variants (reachable `todo!()` from API input) are removed. Timeline `min_fact` is replaced by a two-level map. Two more "two ways to do one task" were removed: `JobClosed` (merge and abandon are already facts) and the `Accepted`/`Fixed` triage verdicts (promotion is the only way to accept a finding; "fixed" is derived from re-anchoring).

**Rejected from candidate-2.**
- Per-repo record-file store: no total order across writers, no store-wide import of existing transcripts, and a snapshot merge that rescans files.
- Process-before-record ordering (run, then write the record): a crash in between repeats the effect. The base writes intent first and dedups by derived id.
- Lease release only in `Drop`: `Drop` does not run on a crash or SIGKILL. The base keeps durable `LeaseOpened`, startup reconciliation, and provider caps.
- Domain types on the wire: a domain rename breaks both frontends. View models stay separate.
- `Topic` as a second list beside the endpoints: in the base any query op can be subscribed, so there is no second list.
- A desk mutex that serializes reads behind writes: the base sequencer serves only short fold reads, and git, LLMs, and processes run off-thread.
- `HashMap`/`HashSet`: iteration order would leak into views and generated output.
- The `LlmClient` HTTP trait: it needs API keys and leaves no raw transcript of the reviewer.
- "Read-only phone by default": superseded by the user's decision that the phone may decide.

## Tradeoffs accepted

- We accept re-decoding raw JSON on every job open in exchange for adapter fixes that repair history. Job views are fully replayed on host start (unverified estimate: well under a second for tens of thousands of records); only `JobStats` is checkpointed, together with its `Applied` state.
- We accept duplicated disk (transcripts copied in, PTY bytes kept for `terminal_days`) in exchange for logs that survive Claude Code's own cleanup and stay complete as assets.
- We accept full LLM prompts stored inside request events in exchange for an audit trail of exactly what each model was asked.
- We accept one sequencer per ledger in exchange for replay order equal to live order and atomic check-then-append (head-only decisions, merge approval).
- We accept recording git readings (hunk headers, blob ids, hunk anchors) in each `Snapshot` in exchange for projections that never call git. They are derived from immutable trees, so they cannot drift.
- We accept a review packet that needs two short reads with `git diff` between them (retried once on `HeadMoved`) in exchange for never holding the job's sequencer while shelling out.
- We accept heuristic provenance (line-hash coverage, shown with its coverage) in exchange for one-click hunk → raw transcript links that need no agent cooperation.
- We accept that feedback to an agent idle at its prompt ends that process and resumes the session (`--resume`) in exchange for one delivery path, one run per round, and at-most-once delivery.
- We accept hand-rolled JSON and `vm_struct!`/`vm_enum!`/`vm_tags!` macros in exchange for std-only and Rust/JSON/TS derived from one declaration.
- We accept agent CLIs for LLM calls (startup latency, output formats owned by the CLI) in exchange for zero key management and a raw transcript of every review.
- We accept polling tailers (about 250 ms) in exchange for no inotify FFI.
- We accept `Cell`-based read tracking inside views (a view set is `!Sync` and lives on its ledger thread) in exchange for subscription dependencies that cannot drift from the code that reads.
- A view-model layer that looks like a duplicate of the views is intentional. Projections are fold state; view models are screens. The host maps one to the other in one file.
- Size: the sketch is about 5.6k lines of Rust, over the 5k target. The growth is the review layer (packet, glance, models, typed view models), not duplication.

## Alternatives considered

- **Decode at ingest and store parsed domain events.** Replay would be cheaper. It lost because there would be two truths: after an adapter fix, parsed events from the old adapter would contradict the raw bytes, and every reader would see adapter versions.
- **Reference transcripts in place instead of copying.** It saves disk. It lost because another program owns the files and may rotate or delete them (unverified: Claude Code's cleanup period), and history analytics is a core feature.
- **Store the review packet (or tiers) at snapshot time.** Reads would be cheaper. It lost because new reviews, checks, threads, and decisions keep arriving after the snapshot, so a stored packet goes stale and becomes a second truth to sync. Composing it at read time costs one memoized diff.
- **Declared per-op subscription dependencies.** This is simpler to explain. It lost because the declaration is a second list next to the op's code and drifts silently. Tracked accessor reads are the declaration.
- **Order-free, commutative projections (CRDT-style, no sequencer).** Multi-host merging would be possible. It lost because every projection would have to handle out-of-order joins, and the head-only verdict check needs an atomic read-then-append.
- **SQLite with mutable state tables.** It gives ad-hoc queries. It lost because mutable rows are a second truth, it adds a native dependency, and the dominant access patterns (fold, tail, range by time) fit logs.
- **Self-report as the main progress source (the dsh model).** It gives rich trees but needs agent cooperation; native plan tools are observable for free.

## Open questions and risks

- Should agents outlive the desktop window? The app embeds the host now, `argusd` is the headless alternative, and the store locks make them exclusive. Would you prefer the app to always talk to a sidecar `argusd`?
- How should the PTY be provided: a `script -qfec` wrapper (std-only), `openpty` FFI (`unsafe`), or tmux (attach from your own terminal, but argus no longer sees the keystrokes)? Do you want to attach to agent terminals from fish at all?
- Is 30 days the right default for pruning terminal logs, and should anything else ever be pruned?
- Phone transport: is WireGuard (the tailnet) enough, or is TLS needed on top? How should pairing tokens rotate? (Decided 2026-11-17: a paired phone may approve, request changes, merge, and answer permission prompts.)
- Transcripts may contain secrets the user pasted; argus copies them verbatim. Is redaction ever acceptable, given that it contradicts "raw means raw"?
- Device checks: should the app build come from a fingerprint-matched EAS build, or from Metro via `--egress local` (unverified)? What default budget, and should auto-run be opt-in per project?
- Should AI review run automatically on every new head (`review_on_snapshot`, off by default because each run costs money), and with which preset?
- Feedback to an agent still sitting at its prompt: end it and `--resume` (current design), or type the feedback into its terminal?
- Merge moves the target ref with a compare-and-swap. If you have the target branch checked out with local changes, should argus refuse, or push a branch / open a PR instead?
- Verified on 92 local transcripts (2026-11-18): 479 record `uuid`s appear in more than one file and none repeats inside one file, so `--resume` does copy history and the record rule is needed. Importing them all applied 109,420 facts, dropped 750 repeated observations, and replayed equal to live. Still unverified: the keystrokes that answer Claude Code's permission prompt in the current version.
- Claude Code splits one API message into one record per content block and repeats `usage` in each, growing while it streams (1,450 of 9,378 messages differ between records). `AgentWhat::Usage` is therefore keyed by message id and views keep the latest value; adding them would double-count tokens.
- About 85% of stored thinking blocks (9,129 of 10,713) have an empty `thinking` and only a signature. The raw view can show that the model thought, but not what, for those turns. Is there a Claude Code setting that keeps the text, or is this a hard limit for feature 5?
- Observed plans are rare in practice: of 92 local transcripts, one uses `TaskCreate`/`TaskUpdate` and none uses `TodoWrite`. Without a human or self-reported plan, `Progress::share` is 0 for most jobs, which reads as "nothing done". Should Glance show "no plan" instead, which means `share()` returns `Option<Share>`? The same run gave 21,117 timeline items, with 9,830 tool calls closed by their results and 3 left open by interrupted sessions.
- Branch points: in 92 local transcripts, 114 records have more than one child, and 99 of them are parallel tool calls (a `tool_use` record whose children are the next `tool_use` and the first `tool_result`). `Transcript::branch_points` therefore counts only children that are conversation (prompt, reply, thinking, tool call), which leaves 8. Unverified: whether all 8 are rewinds or regenerated replies.
- Token totals: every job view over a copy of the same 92 files counted 9,257,869 output tokens, equal to an independent count of the last usage per message per file in Python.
- Claude Code flags (2.1.303 `--help`): feedback delivery runs `claude --resume <prev> --fork-session --session-id <run>`, so every run has its own transcript file. Verified 2026-11-20 with real runs under the PTY: derived (v8) run ids are accepted as session ids, and the agent answers and takes typed input. Still unverified: that `--fork-session` accepts `--session-id`, and that print mode denies a Bash call outside `--allowedTools` instead of waiting.
- Interactive Claude Code sessions wrote no transcript on this machine (2026-11-20): started with a prompt argument or with a typed prompt, under `/tmp` or `~/.cache`, ended by Ctrl-C or `/exit`, none left `~/.claude/projects/<slug>/<id>.jsonl`, while `claude -p --session-id <id>` did, and the user's own `claude -c` sessions do. `~/.claude.json` records the sessions (id, cost) and has `remoteControlAtStartup: true`; whether Remote Control is the cause is unverified. Argus reads agents through their transcripts, so this blocks the interactive agent path until it is understood.
- Later the same day, under 2.1.329, an interactive run with `{"remoteControlAtStartup": false}` in the directory's `.claude/settings.local.json` wrote its transcript (the prompt, the reply, and new record types such as `api-request` and `session-owner`). A second 2.1.329 run with Remote Control left on wrote its transcript too, so Remote Control was not the cause. The version change is the likely one; what else differed between the two days is unverified. Argus therefore needs no Remote Control setting. Project or local settings can only turn Remote Control off, never on, and they load once the folder is trusted. The 2.1.329 trust prompt has no numbers: the selected line is `❯ Yes, I trust this folder` after one Down. Two Ctrl-C presses 1.5 s apart did not end the session; it needed SIGTERM.
- A new worktree shows Claude's folder-trust prompt, with "No, exit" selected. Argus answers it with keys (decided 2026-11-20): wait until the prompt is drawn, send Down, and Enter once the marker is on "Yes". Keys sent before the prompt is drawn are misread.
- A test that started real `claude` and then panicked left it running, with its plugins' language servers; three of them preceded a crash of the machine (14:28, after journald reported memory pressure; the cause is not proven). A dropped `Running` now stops its process.
- The planned TLC command (`java -cp tla2tools.jar tlc2.TLC -tool -noGenerateSpecTE ...` in the spec's directory) and the Lean files script were run by hand on this repository's specs and a copy of `proofs/`: TLC printed 17 messages ending in success, and the script checked every file and exited 1 for the broken one.
- Mutation testing (2026-11-20, cargo-mutants 27, `tools/mutants`), following Petrovic and Ivankovic, "State of Mutation Testing at Google" (ICSE-SEIP 2018): the first run scored 80.5% (1,310 of 1,628 viable mutants killed; 83.3% without `todo!` sketches). Closing the gaps it showed, file by file, brought the workspace to 93.0% (1,477 of 1,589; 96.5% without sketches). The 54 left are equivalent mutants (`|` vs `^` on disjoint bits, empty frames), paths that need fault injection (`EINTR`, I/O errors other than not-found, fsync), code waiting on unwritten adapters (EAS leases, permission keys, insights), or arid ones (Debug output, version constants). The gaps it found were real: path validation that accepted `a//b`, timestamps with any separator, an unpinned hunk fingerprint (stored in snapshots, so a change would unsee every hunk), line ranges at hunk edges, and the review admission rule.
- Mutation testing once logged the desktop session out: a mutant made `runner::children` return `[1]`, and `kill -TERM -- -1` signals every process the user owns. `signal_group` now refuses groups 0 and 1 and the host's own group (`may_signal`), and its test never calls `kill`. Code that signals, deletes, or writes outside a scratch directory must be safe under any single mutation.
- Merge into a checked-out target (decided 2026-11-20): a clean checkout is fast-forwarded there (`git merge --ff-only`), as the user would; one with local changes is refused and nothing moves. A target nobody has checked out moves by `update-ref <new> <old>`. The merge commit is authored by the repository's configured user, since the human approved it. Is that right, or should argus push a branch or open a PR instead?
- Insight prompts over long periods will exceed context. Should they be chunked per job and then merged (two-stage runs)?
- Should argus reuse tlars' message catalog (`tlars-msg`) through a cross-repo path dependency, or vendor the few TLC codes it needs?
- Which agent comes next: Codex's real adapter, OpenCode, Gemini, or ACP as a common protocol?
- CI: `argus-app` (Tauri v2) is outside the std-only workspace, so CI must build it in a separate job with network access (`cargo check --manifest-path crates/argus-app/Cargo.toml`), next to the workspace check, clippy, `pnpm gen:check`, and `tsc --strict`.
- Desktop renderer: revisit [react-native-gpui](https://github.com/streetwriters/react-native-gpui) (React Native 0.87 on Zed's GPUI) when it leaves alpha. It would let the desktop and Expo apps share components and draw large diffs on the GPU. As of 2026-11-18 it is six weeks old, IME composition is untested, its docs disagree on whether a web view exists, and it targets RN 0.87 while `apps/mobile` is on 0.88. View logic stays in `@argus/client`, so a switch would replace only the component layer.

## Next implementation step

Every view in `JobViews` folds facts; the glance, the review packet, `policy::decide`, the Claude, TLC, and Lean commands, the runner (PTY, capture, watch, cancel, input), snapshots, merges, and a job's ledger loop are done. Next: `Host::start` and the API handler, so the desktop app can drive a job. Codex, EAS device checks, and permission keystrokes wait for a verified CLI. `JobStats` checkpoints, `Analytics::slice`, and `Period::parse` wait for the analytics screen.
