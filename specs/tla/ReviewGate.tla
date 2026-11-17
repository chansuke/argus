---------------------------- MODULE ReviewGate ----------------------------
(***************************************************************************)
(* The review gate and the feedback loop.                                  *)
(*                                                                         *)
(* Snapshots 1..head are trees the agent produced; head is the latest. A  *)
(* decision is <<tree, kind>>. Merge needs an ApprovedHead: an Approve     *)
(* decision on the CURRENT head, so a new snapshot voids older approvals. *)
(*                                                                         *)
(* RequestChanges is delivered by a policy run whose id is the decision   *)
(* itself, so requesting it twice is a no-op. RunStarted is durable       *)
(* before the effect; an agent run interrupted by a crash is abandoned,   *)
(* never retried (OnInterrupt::Abandon), so feedback is sent at most once. *)
(*                                                                         *)
(* Mutants (tools/tla-check expects TLC to fail on them):                  *)
(*   HeadOnly = FALSE   merge with any earlier approval                   *)
(*   RetryAgent = TRUE  retry an interrupted delivery                      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS MaxSnapshots, MaxCrashes, HeadOnly, RetryAgent

VARIABLES head, decisions, merged, mergedHead,
          requested, started, sent, attempted, finished, abandoned, up, crashes

vars == <<head, decisions, merged, mergedHead,
          requested, started, sent, attempted, finished, abandoned, up, crashes>>

Trees == 1..MaxSnapshots
Kinds == {"Approve", "RequestChanges"}
DecisionIds == Trees \X Kinds
Feedback == {d \in DecisionIds : d[2] = "RequestChanges"}

TypeOK ==
  /\ head \in 0..MaxSnapshots
  /\ decisions \subseteq DecisionIds
  /\ merged \in 0..MaxSnapshots
  /\ mergedHead \in 0..MaxSnapshots
  /\ requested \subseteq Feedback
  /\ started \subseteq requested
  /\ sent \in [Feedback -> 0..3]
  /\ attempted \subseteq started
  /\ finished \subseteq started
  /\ abandoned \subseteq started
  /\ up \in BOOLEAN
  /\ crashes \in 0..MaxCrashes

Init ==
  /\ head = 0
  /\ decisions = {}
  /\ merged = 0
  /\ mergedHead = 0
  /\ requested = {}
  /\ started = {}
  /\ sent = [d \in Feedback |-> 0]
  /\ attempted = {}
  /\ finished = {}
  /\ abandoned = {}
  /\ up = TRUE
  /\ crashes = 0

Snapshot ==
  /\ up /\ merged = 0 /\ head < MaxSnapshots
  /\ head' = head + 1
  /\ UNCHANGED <<decisions, merged, mergedHead, requested, started, sent, attempted,
                 finished, abandoned, up, crashes>>

\* The human (desktop or phone) decides on the head only.
Decide(k) ==
  /\ up /\ merged = 0 /\ head > 0
  /\ decisions' = decisions \cup {<<head, k>>}
  /\ UNCHANGED <<head, merged, mergedHead, requested, started, sent, attempted,
                 finished, abandoned, up, crashes>>

Merge(t) ==
  /\ up /\ merged = 0
  /\ <<t, "Approve">> \in decisions
  /\ HeadOnly => t = head
  /\ merged' = t
  /\ mergedHead' = head
  /\ UNCHANGED <<head, decisions, requested, started, sent, attempted,
                 finished, abandoned, up, crashes>>

\* Policy: request the delivery run whose id is the decision.
Request(d) ==
  /\ up /\ d \in decisions /\ d \in Feedback /\ d \notin requested
  /\ requested' = requested \cup {d}
  /\ UNCHANGED <<head, decisions, merged, mergedHead, started, sent, attempted,
                 finished, abandoned, up, crashes>>

\* RunStarted is appended before the agent is resumed.
StartRun(d) ==
  /\ up /\ d \in requested /\ d \notin started
  /\ started' = started \cup {d}
  /\ UNCHANGED <<head, decisions, merged, mergedHead, requested, sent, attempted,
                 finished, abandoned, up, crashes>>

\* The effect: the agent receives the feedback text.
Send(d) ==
  /\ up /\ d \in started /\ d \notin finished /\ d \notin abandoned
  /\ d \notin attempted
  /\ sent' = [sent EXCEPT ![d] = @ + 1]
  /\ attempted' = attempted \cup {d}
  /\ UNCHANGED <<head, decisions, merged, mergedHead, requested, started,
                 finished, abandoned, up, crashes>>

Finish(d) ==
  /\ up /\ d \in started /\ d \notin finished /\ d \notin abandoned
  /\ d \in attempted
  /\ finished' = finished \cup {d}
  /\ UNCHANGED <<head, decisions, merged, mergedHead, requested, started,
                 sent, attempted, abandoned, up, crashes>>

\* On a crash, every started-but-unfinished agent run is abandoned, unless
\* the RetryAgent mutant lets it run again after restart.
Crash ==
  /\ up /\ crashes < MaxCrashes
  /\ up' = FALSE
  /\ crashes' = crashes + 1
  /\ abandoned' = IF RetryAgent THEN abandoned ELSE abandoned \cup (started \ finished)
  \* A retried run is a new attempt, so its effect may happen again.
  /\ attempted' = IF RetryAgent THEN attempted \cap finished ELSE attempted
  /\ UNCHANGED <<head, decisions, merged, mergedHead, requested, started,
                 sent, finished>>

Restart ==
  /\ ~up
  /\ up' = TRUE
  /\ UNCHANGED <<head, decisions, merged, mergedHead, requested, started,
                 sent, attempted, finished, abandoned, crashes>>

Next ==
  \/ Snapshot
  \/ \E k \in Kinds : Decide(k)
  \/ \E t \in Trees : Merge(t)
  \/ \E d \in Feedback : Request(d) \/ StartRun(d) \/ Send(d) \/ Finish(d)
  \/ Crash
  \/ Restart

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(Restart)
  /\ \A d \in Feedback :
       WF_vars(Request(d)) /\ WF_vars(StartRun(d)) /\ WF_vars(Send(d)) /\ WF_vars(Finish(d))

\* What was merged was approved, and was the head when it was merged.
MergedApprovedHead ==
  merged /= 0 => (<<merged, "Approve">> \in decisions /\ merged = mergedHead)

\* Feedback reaches the agent at most once per decision.
AtMostOnce == \A d \in Feedback : sent[d] <= 1

\* Every RequestChanges is eventually delivered or visibly abandoned.
FeedbackResolves ==
  \A d \in Feedback : d \in decisions ~> (d \in finished \/ d \in abandoned)
=============================================================================
