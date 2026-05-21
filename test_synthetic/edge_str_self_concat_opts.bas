10 REM String opt 1+2: self-append (var$=var$+rhs$) + LASTVAR rewind
20 REM Verifies in-place append works correctly across many iters.
30 A$="":B$="AB"
40 FOR I=1 TO 20:A$=A$+B$:NEXT
50 PRINT LEN(A$);A$
60 REM Verify the chunk's owner backref still points at A$.
70 REM (If LASTVAR rewind corrupted the chunk, reading A$ would
80 REM crash or return garbage.)
90 PRINT MID$(A$,1,4)
100 PRINT MID$(A$,LEN(A$)-3,4)
110 REM Self-prepend (var$=rhs$+var$) — opt 2's LASTVAR rewind must
120 REM NOT fire here (LHS != self var).
130 C$="":FOR I=1 TO 5:C$="X"+C$:NEXT
140 PRINT LEN(C$);C$
