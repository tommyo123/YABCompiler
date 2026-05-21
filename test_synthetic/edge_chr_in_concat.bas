10 REM CHR$ inside string concat — must NOT take CharOut path
20 N=65
30 A$=CHR$(N)+CHR$(N+1)+CHR$(N+2)
40 PRINT A$
50 REM Mixed: print CHR$ var and concat result on same line
60 PRINT A$;CHR$(N+3)
70 REM CHR$ of LEN/ASC inside concat
80 B$="X"
90 C$="["+CHR$(LEN(B$)+48)+"]"
100 PRINT C$
110 REM Reassign A$ to test heap GC interaction
120 A$=CHR$(48)+CHR$(49)+CHR$(50)
130 PRINT A$
