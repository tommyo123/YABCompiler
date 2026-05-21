start tok64 dim_dyn.prg
10 n = 7
20 dim a(n)
30 for i=0 to n
40 a(i) = i*i
50 next i
60 for i=0 to n
70 print "a("; i; ")="; a(i)
80 next i
90 m = 3 : k = 2
100 dim b%(m, k)
110 b%(0,0) = 11
120 b%(m, k) = -42
130 print "b%(0,0)="; b%(0,0); " b%(m,k)="; b%(m,k)
