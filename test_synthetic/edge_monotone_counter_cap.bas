10 REM Monotone counter cap: dataflow threshold widening
20 REM should bound X by M (the eq-exit value) so X gets u8 storage
30 REM and the inner loop uses byte ops + abs,X indexing.
40 M=100:X=-1
50 X=X+1:IFX=MTHEN70
60 GOTO50
70 PRINT"X=";X;" M=";M
