10 REM Regression: int-FOR with end > 255 must use 16-bit compare in IF
20 REM Iter counts kept low so cart-BASIC interpreter finishes in 4s
30 C=0:FOR I=1 TO 300:IF I>250 THEN C=C+1
40 NEXT I:PRINT "C=";C
50 D=0:FOR I=250 TO 320:IF I<300 THEN D=D+1
60 NEXT I:PRINT "D=";D
70 E=0:FOR I=290 TO 310:IF I=300 THEN E=E+1
80 NEXT I:PRINT "E=";E
