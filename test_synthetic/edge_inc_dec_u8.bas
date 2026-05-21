10 REM INC/DEC u8 fast path: counter +/- 1 + IF=0 → DEC m / BEQ
20 REM exercises ph028 (drop LDA after INC/DEC when A dead)
30 M=10
40 M=M-1:IFM=.THEN70
50 PRINT M;
60 GOTO 40
70 PRINT "DONE M=";M
80 N=0
90 N=N+1:IFN=5THEN110
100 GOTO 90
110 PRINT"N=";N
