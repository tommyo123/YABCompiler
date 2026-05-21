start tok64 run_test.prg
10 if ti > 200 then 100
20 c = c + 1
30 a$ = a$ + "x"
40 dim n(2)
50 n(0) = n(0) + 7
60 run
100 print "after run:"
110 print "c="; c
120 print "a$=("; a$;")"
130 print "n(0)="; n(0)
140 print "n(1)="; n(1)
