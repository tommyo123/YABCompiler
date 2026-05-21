start tok64 clr_test.prg
10 a = 42
20 b$ = "hello"
30 dim n(3)
40 n(0) = 7
50 n(1) = 14
60 print "before clr: a="; a; " b$="; b$; " n(0)="; n(0); " n(1)="; n(1)
70 clr
80 print "after clr:  a="; a; " b$=("; b$;") n(0)="; n(0); " n(1)="; n(1)
90 a = 99
100 print "after re-assign: a="; a
