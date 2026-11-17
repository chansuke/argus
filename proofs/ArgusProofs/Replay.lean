/-!
# Replay idempotence of argus-log cursors

`argus-log` stores one append-only log per actor. A frame's identity is
`(actor, seq)`, where `seq` is its dense position in that actor's log
(starting at 1). Views fold frames through a cursor that remembers, per
actor, the highest `seq` already applied, and admits a frame only when its
`seq` is above that.

This file proves the property the Rust `Cursor::admit` relies on: replaying
any frames that were already folded, in any order and any number of times,
leaves the state unchanged. That is what makes re-delivery after a crash, a
tailer re-reading a file, or a second subscription a no-op.
-/

namespace Argus

structure Frame where
  actor : Nat
  seq : Nat
  deriving DecidableEq

structure State where
  cursor : Nat → Nat
  applied : List Frame

/-- `Cursor::admit` followed by `Projection::apply`. -/
def step (s : State) (f : Frame) : State :=
  if s.cursor f.actor < f.seq then
    { cursor := fun a => if a = f.actor then f.seq else s.cursor a
      applied := s.applied ++ [f] }
  else s

def replay (s : State) (fs : List Frame) : State :=
  fs.foldl step s

theorem step_cursor_mono (s : State) (f : Frame) (a : Nat) :
    s.cursor a ≤ (step s f).cursor a := by
  unfold step
  split
  · rename_i h
    show s.cursor a ≤ (if a = f.actor then f.seq else s.cursor a)
    split
    · rename_i ha
      subst ha
      exact Nat.le_of_lt h
    · exact Nat.le_refl _
  · exact Nat.le_refl _

theorem step_covers (s : State) (f : Frame) : f.seq ≤ (step s f).cursor f.actor := by
  unfold step
  split
  · simp
  · rename_i h
    exact Nat.le_of_not_lt h

theorem replay_cursor_mono (fs : List Frame) :
    ∀ (s : State) (a : Nat), s.cursor a ≤ (replay s fs).cursor a := by
  induction fs with
  | nil => intro s a; exact Nat.le_refl _
  | cons f fs ih =>
    intro s a
    exact Nat.le_trans (step_cursor_mono s f a) (ih (step s f) a)

/-- After a replay, every replayed frame is at or below its actor's cursor. -/
theorem replay_covers (fs : List Frame) :
    ∀ (s : State), ∀ f ∈ fs, f.seq ≤ (replay s fs).cursor f.actor := by
  induction fs with
  | nil => intro s f hf; cases hf
  | cons g fs ih =>
    intro s f hf
    cases hf with
    | head => exact Nat.le_trans (step_covers s g) (replay_cursor_mono fs (step s g) g.actor)
    | tail _ hf => exact ih (step s g) f hf

/-- A frame at or below its actor's cursor is dropped without changing anything. -/
theorem step_noop (s : State) (f : Frame) (h : f.seq ≤ s.cursor f.actor) : step s f = s := by
  unfold step
  split
  · rename_i hlt
    exact absurd hlt (Nat.not_lt.mpr h)
  · rfl

theorem replay_noop (fs : List Frame) :
    ∀ (s : State), (∀ f ∈ fs, f.seq ≤ s.cursor f.actor) → replay s fs = s := by
  induction fs with
  | nil => intro s _; rfl
  | cons f fs ih =>
    intro s h
    show replay (step s f) fs = s
    rw [step_noop s f (h f (List.Mem.head _))]
    exact ih s (fun g hg => h g (List.Mem.tail _ hg))

/-- Replaying any frames already folded, in any order, is a no-op. -/
theorem replay_idempotent (s : State) (fs gs : List Frame) (h : ∀ g ∈ gs, g ∈ fs) :
    replay (replay s fs) gs = replay s fs :=
  replay_noop gs (replay s fs) (fun g hg => replay_covers fs s g (h g hg))

/-- In particular, folding the same frames twice equals folding them once. -/
theorem replay_twice (s : State) (fs : List Frame) :
    replay (replay s fs) fs = replay s fs :=
  replay_idempotent s fs fs (fun _ h => h)

/-- Duplicated delivery (`fs ++ fs`, as after a tailer re-read) changes nothing. -/
theorem replay_duplicated (s : State) (fs : List Frame) :
    replay s (fs ++ fs) = replay s fs := by
  unfold replay
  rw [List.foldl_append]
  exact replay_twice s fs

end Argus
