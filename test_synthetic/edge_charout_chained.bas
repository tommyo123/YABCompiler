10 REM CharOut chains — many in one PRINT statement
20 PRINT CHR$(13);CHR$(13);"AFTER 2 NEWLINES"
30 REM Mix of literal and var CHR$
40 N=42
50 PRINT CHR$(N);CHR$(N+1);CHR$(N+2);CHR$(N+3)
60 REM CHR$ from byte var inside loop
70 FOR I=0 TO 9
80 K=48+I
90 PRINT CHR$(K);
100 NEXT:PRINT
110 REM CHR$ followed by string concat
120 A$="WORLD"
130 PRINT CHR$(72);CHR$(73);" ";A$
