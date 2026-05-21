10 REM Dead-store string with FRE in program: must NOT drop.
20 REM Behaviorally correct either way, but verifies the pass
30 REM doesn't crash and produces correct output.
40 A$="X"
50 A$=A$+"DEAD"
60 F=FRE(0)
70 A$="FINAL"
80 PRINT A$
