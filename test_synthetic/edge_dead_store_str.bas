10 REM Dead-store string: heap-allocating dead store should be
20 REM eliminated when no FRE is in the program.
30 A$="INITIAL"
40 A$=A$+"_DEAD"
50 A$="FINAL"
60 PRINT A$
