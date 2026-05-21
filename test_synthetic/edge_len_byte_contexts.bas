10 REM LEN used as byte in many contexts
20 A$="HELLO WORLD":B$="HI"
30 REM LEN compared to literal — IF condition byte path
40 IF LEN(A$)>5 THEN PRINT "LONG"
50 IF LEN(B$)<5 THEN PRINT "SHORT"
60 REM LEN as POKE value
70 POKE 1024,LEN(A$)
80 PRINT PEEK(1024)
90 REM LEN as FOR limit
100 FOR I=1 TO LEN(B$)
110 PRINT MID$(B$,I,1);
120 NEXT:PRINT
130 REM LEN of empty
140 E$=""
150 PRINT LEN(E$)
160 REM LEN in arithmetic
170 PRINT LEN(A$)+LEN(B$)
180 REM LEN of concat (heap)
190 PRINT LEN(A$+B$)
