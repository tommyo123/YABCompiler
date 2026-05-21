10 REM PokeLoopFusion already handles const-value FOR-POKE.
20 REM Verify the variable-value variant (value depends on I)
30 REM still works through the general FOR-POKE path.
40 W=1024
50 FOR I=0 TO 9:POKE W+I,65+I:NEXT
60 FOR I=0 TO 9:PRINT CHR$(PEEK(W+I));:NEXT
70 PRINT
80 REM Also exercise PokeLoopFusion's const-value path.
90 FOR I=0 TO 9:POKE W+I+10,32:NEXT
100 FOR I=10 TO 19:PRINT CHR$(PEEK(W+I));:NEXT
110 PRINT "END"
