10 REM GosubShortBodyInline must NOT inline a >2-stmt body. Verify
20 REM the program still works correctly (i.e. body kept as JSR).
30 X=1:GOSUB 100
40 X=2:GOSUB 100
50 X=3:GOSUB 100
60 PRINT"DONE T=";T
70 END
100 T=T+X:T=T+1:T=T+2:T=T+3:RETURN
