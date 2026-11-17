---- MODULE ReviewGate_TTrace_1794926647 ----
EXTENDS Sequences, TLCExt, ReviewGate, Toolbox, Naturals, TLC

_expression ==
    LET ReviewGate_TEExpression == INSTANCE ReviewGate_TEExpression
    IN ReviewGate_TEExpression!expression
----

_trace ==
    LET ReviewGate_TETrace == INSTANCE ReviewGate_TETrace
    IN ReviewGate_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        head = (2)
        /\
        requested = ({})
        /\
        mergedHead = (2)
        /\
        merged = (1)
        /\
        decisions = ({<<1, "Approve">>})
        /\
        started = ({})
        /\
        finished = ({})
        /\
        crashes = (0)
        /\
        attempted = ({})
        /\
        up = (TRUE)
        /\
        abandoned = ({})
        /\
        sent = ((<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0))
    )
----

_init ==
    /\ decisions = _TETrace[1].decisions
    /\ merged = _TETrace[1].merged
    /\ abandoned = _TETrace[1].abandoned
    /\ started = _TETrace[1].started
    /\ crashes = _TETrace[1].crashes
    /\ attempted = _TETrace[1].attempted
    /\ up = _TETrace[1].up
    /\ sent = _TETrace[1].sent
    /\ head = _TETrace[1].head
    /\ mergedHead = _TETrace[1].mergedHead
    /\ finished = _TETrace[1].finished
    /\ requested = _TETrace[1].requested
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ decisions  = _TETrace[i].decisions
        /\ decisions' = _TETrace[j].decisions
        /\ merged  = _TETrace[i].merged
        /\ merged' = _TETrace[j].merged
        /\ abandoned  = _TETrace[i].abandoned
        /\ abandoned' = _TETrace[j].abandoned
        /\ started  = _TETrace[i].started
        /\ started' = _TETrace[j].started
        /\ crashes  = _TETrace[i].crashes
        /\ crashes' = _TETrace[j].crashes
        /\ attempted  = _TETrace[i].attempted
        /\ attempted' = _TETrace[j].attempted
        /\ up  = _TETrace[i].up
        /\ up' = _TETrace[j].up
        /\ sent  = _TETrace[i].sent
        /\ sent' = _TETrace[j].sent
        /\ head  = _TETrace[i].head
        /\ head' = _TETrace[j].head
        /\ mergedHead  = _TETrace[i].mergedHead
        /\ mergedHead' = _TETrace[j].mergedHead
        /\ finished  = _TETrace[i].finished
        /\ finished' = _TETrace[j].finished
        /\ requested  = _TETrace[i].requested
        /\ requested' = _TETrace[j].requested

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("ReviewGate_TTrace_1794926647.json", _TETrace)

=============================================================================

 Note that you can extract this module `ReviewGate_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `ReviewGate_TEExpression.tla` file takes precedence 
  over the module `ReviewGate_TEExpression` below).

---- MODULE ReviewGate_TEExpression ----
EXTENDS Sequences, TLCExt, ReviewGate, Toolbox, Naturals, TLC

expression == 
    [
        \* To hide variables of the `ReviewGate` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        decisions |-> decisions
        ,merged |-> merged
        ,abandoned |-> abandoned
        ,started |-> started
        ,crashes |-> crashes
        ,attempted |-> attempted
        ,up |-> up
        ,sent |-> sent
        ,head |-> head
        ,mergedHead |-> mergedHead
        ,finished |-> finished
        ,requested |-> requested
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_decisionsUnchanged |-> decisions = decisions'
        
        \* Format the `decisions` variable as Json value.
        \* ,_decisionsJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(decisions)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_decisionsModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].decisions # _TETrace[s-1].decisions
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE ReviewGate_TETrace ----
\*EXTENDS IOUtils, ReviewGate, TLC
\*
\*trace == IODeserialize("ReviewGate_TTrace_1794926647.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE ReviewGate_TETrace ----
EXTENDS ReviewGate, TLC

trace == 
    <<
    ([head |-> 0,requested |-> {},mergedHead |-> 0,merged |-> 0,decisions |-> {},started |-> {},finished |-> {},crashes |-> 0,attempted |-> {},up |-> TRUE,abandoned |-> {},sent |-> (<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0)]),
    ([head |-> 1,requested |-> {},mergedHead |-> 0,merged |-> 0,decisions |-> {},started |-> {},finished |-> {},crashes |-> 0,attempted |-> {},up |-> TRUE,abandoned |-> {},sent |-> (<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0)]),
    ([head |-> 1,requested |-> {},mergedHead |-> 0,merged |-> 0,decisions |-> {<<1, "Approve">>},started |-> {},finished |-> {},crashes |-> 0,attempted |-> {},up |-> TRUE,abandoned |-> {},sent |-> (<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0)]),
    ([head |-> 2,requested |-> {},mergedHead |-> 0,merged |-> 0,decisions |-> {<<1, "Approve">>},started |-> {},finished |-> {},crashes |-> 0,attempted |-> {},up |-> TRUE,abandoned |-> {},sent |-> (<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0)]),
    ([head |-> 2,requested |-> {},mergedHead |-> 2,merged |-> 1,decisions |-> {<<1, "Approve">>},started |-> {},finished |-> {},crashes |-> 0,attempted |-> {},up |-> TRUE,abandoned |-> {},sent |-> (<<1, "RequestChanges">> :> 0 @@ <<2, "RequestChanges">> :> 0 @@ <<3, "RequestChanges">> :> 0)])
    >>
----


=============================================================================

---- CONFIG ReviewGate_TTrace_1794926647 ----
CONSTANTS
    MaxSnapshots = 3
    MaxCrashes = 2
    HeadOnly = FALSE
    RetryAgent = FALSE

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
\* Generated on Tue Nov 17 23:44:08 JST 2026