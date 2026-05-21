10 REM CHR$(ASC(...)) chains both byte fast paths
20 A$="HELLO"
30 PRINT CHR$(ASC(A$))
40 REM CHR$(ASC + offset) — case shift
50 B$="hello"
60 FOR I=1 TO LEN(B$)
70 PRINT CHR$(ASC(MID$(B$,I,1))-32);
80 NEXT:PRINT
90 REM CHR$(LEN) — print a digit char from length
100 C$="ABCD"
110 PRINT CHR$(LEN(C$)+48)
120 PRINT CHR$(LEN("")+48)
