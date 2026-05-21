10 REM GosubShortBodyInline: small 2-stmt body called multiple times
20 REM should be inlined at every call site.
30 X=10:GOSUB 100
40 X=20:GOSUB 100
50 X=30:GOSUB 100
60 X=40:GOSUB 100
70 PRINT"X=";X;" S=";S
80 END
100 S=S+X:X=X*2:RETURN
