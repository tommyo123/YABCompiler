start tok-simon tsb_low_hanging.prg
10 cls
20 d!poke 1024,513
30 print d!peek(1024)
40 colour 0,6,1
50 color 6
60 vol 5
70 mob 0 on
80 mob 0 off
90 mobcol 0,2
100 cmob 3,4
110 print penx;peny
120 print frac(3.5)
130 print exor(5,3)
140 print $d020;%1010;$$00ff;%%1111
150 print joy(1)
160 pause 1
170 print "done"
