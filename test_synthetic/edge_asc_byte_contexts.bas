10 REM ASC in byte contexts — must trap on empty
20 A$="A":B$="Z"
30 PRINT ASC(A$);ASC(B$)
40 REM ASC compared to literal
50 IF ASC(A$)=65 THEN PRINT "MATCH"
60 IF ASC(B$)<>90 THEN PRINT "BAD"
70 REM ASC arithmetic
80 PRINT ASC("A")+1
90 REM ASC as POKE value
100 POKE 1025,ASC(A$)
110 PRINT PEEK(1025)
120 REM ASC of long string returns first char only
130 C$="HELLO"
140 PRINT ASC(C$)
150 REM ASC chained with CHR$ — identity for printable
160 PRINT CHR$(ASC("X"))
