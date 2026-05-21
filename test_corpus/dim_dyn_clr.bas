start tok64 dim_dyn_clr.prg
10 n = 5
20 dim a(n)
30 for i=0 to n
40 a(i) = i + 100
50 next i
60 print "before clr: a(0)="; a(0); " a(5)="; a(5)
70 clr
80 n = 3
90 dim a(n)
100 for i=0 to n
110 a(i) = i + 200
120 next i
130 print "after clr: a(0)="; a(0); " a(3)="; a(3)
