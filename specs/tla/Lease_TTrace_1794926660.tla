---- MODULE Lease_TTrace_1794926660 ----
EXTENDS Sequences, TLCExt, Lease_TEConstants, Lease, Toolbox, Naturals, TLC

_expression ==
    LET Lease_TEExpression == INSTANCE Lease_TEExpression
    IN Lease_TEExpression!expression
----

_trace ==
    LET Lease_TETrace == INSTANCE Lease_TETrace
    IN Lease_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        ledger = ((l1 :> "none" @@ l2 :> "none"))
        /\
        crashes = (0)
        /\
        up = (TRUE)
        /\
        remote = ((l1 :> "running" @@ l2 :> "absent"))
    )
----

_init ==
    /\ remote = _TETrace[1].remote
    /\ crashes = _TETrace[1].crashes
    /\ up = _TETrace[1].up
    /\ ledger = _TETrace[1].ledger
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ remote  = _TETrace[i].remote
        /\ remote' = _TETrace[j].remote
        /\ crashes  = _TETrace[i].crashes
        /\ crashes' = _TETrace[j].crashes
        /\ up  = _TETrace[i].up
        /\ up' = _TETrace[j].up
        /\ ledger  = _TETrace[i].ledger
        /\ ledger' = _TETrace[j].ledger

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("Lease_TTrace_1794926660.json", _TETrace)

=============================================================================

 Note that you can extract this module `Lease_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `Lease_TEExpression.tla` file takes precedence 
  over the module `Lease_TEExpression` below).

---- MODULE Lease_TEExpression ----
EXTENDS Sequences, TLCExt, Lease_TEConstants, Lease, Toolbox, Naturals, TLC

expression == 
    [
        \* To hide variables of the `Lease` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        remote |-> remote
        ,crashes |-> crashes
        ,up |-> up
        ,ledger |-> ledger
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_remoteUnchanged |-> remote = remote'
        
        \* Format the `remote` variable as Json value.
        \* ,_remoteJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(remote)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_remoteModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].remote # _TETrace[s-1].remote
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE Lease_TETrace ----
\*EXTENDS IOUtils, Lease_TEConstants, Lease, TLC
\*
\*trace == IODeserialize("Lease_TTrace_1794926660.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE Lease_TETrace ----
EXTENDS Lease_TEConstants, Lease, TLC

trace == 
    <<
    ([ledger |-> (l1 :> "none" @@ l2 :> "none"),crashes |-> 0,up |-> TRUE,remote |-> (l1 :> "absent" @@ l2 :> "absent")]),
    ([ledger |-> (l1 :> "none" @@ l2 :> "none"),crashes |-> 0,up |-> TRUE,remote |-> (l1 :> "running" @@ l2 :> "absent")])
    >>
----


=============================================================================

---- MODULE Lease_TEConstants ----
EXTENDS Lease

CONSTANTS l1, l2

=============================================================================

---- CONFIG Lease_TTrace_1794926660 ----
CONSTANTS
    Leases = { l1 , l2 }
    MaxCrashes = 2
    RecordFirst = FALSE
    l2 = l2
    l1 = l1

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Tue Nov 17 23:44:21 JST 2026