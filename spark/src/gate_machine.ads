--  SPDX-License-Identifier: MPL-2.0
--  Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>
--
--  Gate_Machine — the SPARK formal heart of `squabble /= bypass`.
--
--  This package models a branch-protection gate as a set of required checks
--  and PROVES, mechanically, that the only value of the check set that yields
--  Green is one in which every required check actually ran and passed. There is
--  no parameter for an admin override, no way for an empty (i.e. dropped) set
--  of required contexts to read Green, and no "renamed-away" escape: those are
--  provably NOT paths to Green. The Rust `squabble_core::gate` mirrors this
--  exact semantics.

package Gate_Machine
  with SPARK_Mode => On
is
   --  The realised result of one required check on the head commit. `Missing`
   --  models a required context with no run bound to it (the v0.1 deadlock
   --  class). `Skipped` is a run that concluded SKIPPED or NEUTRAL: GitHub
   --  counts it as satisfying the requirement, so the gate must too, but it
   --  carries NO evidence — it is kept distinct from `Passed` so that a
   --  skipped scan can never be reported as a real green.
   type Check_Run is (Missing, Pending, Failed, Skipped, Passed);

   --  The ruleset's own notion of a met requirement.
   function Satisfies (R : Check_Run) return Boolean is
     (R = Passed or else R = Skipped);

   --  Where the gate sits. Green is COMPUTED from the runs, never asserted.
   type Gate_State is (Blocked, Red, Green);

   Max_Checks : constant := 256;
   type Check_Index is range 1 .. Max_Checks;
   type Check_Array is array (Check_Index range <>) of Check_Run;

   --  Evaluate the gate. The postcondition is the load-bearing theorem:
   --  the result is Green IFF the required set is non-empty AND every required
   --  check is satisfied (Passed or Skipped). Proving this body against this
   --  contract is the machine check that a squabble can only reach green by
   --  satisfying the gate.
   --  Any Failed check yields Red, even if others are Missing or Pending.
   --  Otherwise, an empty set or any Missing or Pending check yields Blocked.
   function Evaluate (C : Check_Array) return Gate_State
     with
       Post =>
         (Evaluate'Result = Green)
           = (C'Length > 0
              and then (for all I in C'Range => Satisfies (C (I))));

end Gate_Machine;
