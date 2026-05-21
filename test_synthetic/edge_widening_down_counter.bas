10 REM Threshold widening exit guard: M=240 down to 0
20 REM should converge with M in u8 range, not float-fallback.
30 M=240:C=0
40 M=M-1
50 IFM=.THEN70
60 C=C+1:GOTO40
70 PRINT"COUNT=";C;" M=";M
