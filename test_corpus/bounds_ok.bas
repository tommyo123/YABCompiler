start tok64 bounds_ok.prg
10 dim a(5), b(2,3)
20 for i=0 to 5
30 a(i) = i*10
40 next i
50 print "1d:";
60 for i=0 to 5
70 print a(i);
80 next i
90 print
100 for i=0 to 2
110 for j=0 to 3
120 b(i,j) = i*100 + j
130 next j
140 next i
150 print "2d:";
160 for i=0 to 2
170 for j=0 to 3
180 print b(i,j);
190 next j
200 next i
210 print
220 print "all in range, done"
