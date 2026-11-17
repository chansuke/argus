------------------------------- MODULE Lease -------------------------------
(***************************************************************************)
(* Billed remote sessions (EAS Simulators) are leases in argus's ledger.  *)
(* argus appends LeaseOpened durably BEFORE it creates the remote session *)
(* (tagged with the lease id), so a host crash at any point leaves a      *)
(* record that the startup reconciler can act on.                          *)
(*                                                                         *)
(* Neither property below relies on the provider's own lifetime cap: that *)
(* action exists but has no fairness, so the host alone must clean up.    *)
(*                                                                         *)
(* RecordFirst = FALSE models the rejected ordering "start the session,   *)
(* then write the record" (see docs/design/RATIONALE.md). TLC must find a *)
(* violation for it; tools/tla-check enforces that.                       *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Leases, MaxCrashes, RecordFirst

VARIABLES ledger, remote, up, crashes

vars == <<ledger, remote, up, crashes>>

LedgerStates == {"none", "opened", "bound", "closed"}
RemoteStates == {"absent", "running", "stopped"}
Open == {"opened", "bound"}

TypeOK ==
  /\ ledger \in [Leases -> LedgerStates]
  /\ remote \in [Leases -> RemoteStates]
  /\ up \in BOOLEAN
  /\ crashes \in 0..MaxCrashes

Init ==
  /\ ledger = [l \in Leases |-> "none"]
  /\ remote = [l \in Leases |-> "absent"]
  /\ up = TRUE
  /\ crashes = 0

\* Append LeaseOpened (fsync). Under the rejected ordering the record is
\* written only after the session already runs.
Record(l) ==
  /\ up
  /\ ledger[l] = "none"
  /\ RecordFirst \/ remote[l] = "running"
  /\ ledger' = [ledger EXCEPT ![l] = "opened"]
  /\ UNCHANGED <<remote, up, crashes>>

\* eas simulator:start --tag <lease>
Start(l) ==
  /\ up
  /\ remote[l] = "absent"
  /\ IF RecordFirst THEN ledger[l] = "opened" ELSE ledger[l] = "none"
  /\ remote' = [remote EXCEPT ![l] = "running"]
  /\ UNCHANGED <<ledger, up, crashes>>

\* Append LeaseBound{remote_id}.
Bind(l) ==
  /\ up
  /\ ledger[l] = "opened"
  /\ remote[l] = "running"
  /\ ledger' = [ledger EXCEPT ![l] = "bound"]
  /\ UNCHANGED <<remote, up, crashes>>

\* The host stops a session it knows from the ledger. An opened-but-unbound
\* lease is found by its tag. This is also what startup reconciliation does.
Stop(l) ==
  /\ up
  /\ ledger[l] \in Open
  /\ remote[l] = "running"
  /\ remote' = [remote EXCEPT ![l] = "stopped"]
  /\ UNCHANGED <<ledger, up, crashes>>

\* Append LeaseClosed once nothing is running.
Close(l) ==
  /\ up
  /\ ledger[l] \in Open
  /\ remote[l] /= "running"
  /\ ledger' = [ledger EXCEPT ![l] = "closed"]
  /\ UNCHANGED <<remote, up, crashes>>

\* --max-duration-minutes on the provider side. No fairness: not relied on.
ProviderCap(l) ==
  /\ remote[l] = "running"
  /\ remote' = [remote EXCEPT ![l] = "stopped"]
  /\ UNCHANGED <<ledger, up, crashes>>

Crash ==
  /\ up
  /\ crashes < MaxCrashes
  /\ up' = FALSE
  /\ crashes' = crashes + 1
  /\ UNCHANGED <<ledger, remote>>

Restart ==
  /\ ~up
  /\ up' = TRUE
  /\ UNCHANGED <<ledger, remote, crashes>>

Next ==
  \/ \E l \in Leases :
       Record(l) \/ Start(l) \/ Bind(l) \/ Stop(l) \/ Close(l) \/ ProviderCap(l)
  \/ Crash
  \/ Restart

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(Restart)
  /\ \A l \in Leases : WF_vars(Stop(l)) /\ WF_vars(Close(l))

\* Every running session is known to the ledger, so the host can stop it.
Tracked == \A l \in Leases : remote[l] = "running" => ledger[l] \in Open

\* A closed lease never has a running session behind it.
ClosedMeansStopped == \A l \in Leases : ledger[l] = "closed" => remote[l] /= "running"

\* Every session that starts is eventually stopped by the host.
EventuallyStopped == \A l \in Leases : remote[l] = "running" ~> remote[l] = "stopped"
=============================================================================
