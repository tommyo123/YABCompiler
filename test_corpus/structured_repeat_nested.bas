start tok-simon structured_repeat_nested.prg
10 t%=0
20 i%=0
30 repeat
40 j%=0
50 repeat
60 t%=t%+1
70 j%=j%+1
80 until j%=3
90 i%=i%+1
100 until i%=4
110 print "t"; t%
