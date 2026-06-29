--  SPDX-License-Identifier: MPL-2.0
--  Copyright (c) 2026 Jonathan D.A. Jewell (hyperpolymath) <j.d.a.jewell@open.ac.uk>

package body Gate_Machine
  with SPARK_Mode => On
is

   function Evaluate (C : Check_Array) return Gate_State is
      All_Pass    : Boolean := True;
      Saw_Failure : Boolean := False;
   begin
      if C'Length = 0 then
         --  No required contexts at all is unprotected, not won.
         return Blocked;
      end if;

      for I in C'Range loop
         if C (I) /= Passed then
            All_Pass := False;
         end if;
         if C (I) = Failed then
            Saw_Failure := True;
         end if;

         pragma Loop_Invariant
           (All_Pass = (for all J in C'First .. I => C (J) = Passed));
         pragma Loop_Invariant
           (Saw_Failure = (for some J in C'First .. I => C (J) = Failed));
      end loop;

      if All_Pass then
         return Green;
      elsif Saw_Failure then
         return Red;
      else
         return Blocked;
      end if;
   end Evaluate;

end Gate_Machine;
