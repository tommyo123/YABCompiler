10 REM Address flattening: PEEK(W+I+1) etc reach abs,X when
20 REM W is constant-folded and I is u8 or path-u8.
30 W=1024:FOR I=0 TO 9:POKE W+I,65+I:NEXT
40 FOR I=0 TO 9:PRINT CHR$(PEEK(W+I));:NEXT
50 PRINT
60 FOR I=0 TO 8:PRINT CHR$(PEEK(W+I+1));:NEXT
70 PRINT
80 FOR I=1 TO 9:PRINT CHR$(PEEK(W+I-1));:NEXT
90 PRINT
