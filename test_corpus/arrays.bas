start tok64 arrays.prg
10 dim a(10), s(5)
20 for i=0 to 10
30 a(i) = i*i
40 next i
50 print "squares:"
60 for i=0 to 10
70 print a(i)
80 next i
90 print "sum of squares:"
100 t = 0
110 for i=0 to 10
120 t = t + a(i)
130 next i
140 print t
150 print "lookup:"
160 s(0) = 100
170 s(1) = 200
180 s(2) = 300
190 print s(0); s(1); s(2); s(0)+s(1)+s(2)
