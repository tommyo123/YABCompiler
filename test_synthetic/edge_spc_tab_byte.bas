10 REM SPC and TAB with byte-fast-path arguments
20 PRINT "A";SPC(0);"B"
30 PRINT "X";SPC(5);"Y"
40 FOR I=1 TO 5
50 PRINT TAB(I*2);"*"
60 NEXT
70 REM SPC with LEN
80 A$="HELLO":B$="WORLD"
90 PRINT A$;SPC(LEN(A$));B$
100 REM TAB with byte var
110 N=15
120 PRINT TAB(N);"COL15"
130 REM TAB with int-island expression
140 PRINT TAB((10+5) AND 31);"X"
150 REM TAB(0) edge — should not move backwards
160 PRINT "ABC";TAB(0);"DEF"
