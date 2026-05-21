10 REM LASTVAR mixed: writes to A$ then B$ then A$ — the B$ write
20 REM should clear LASTVAR (or simply mismatch), so the second
30 REM A$ write does NOT rewind heap past B$'s chunk.
40 A$="ALPHA"
50 A$=A$+"!"
60 B$=A$+"X"
70 A$=A$+"?"
80 PRINT A$
90 PRINT B$
