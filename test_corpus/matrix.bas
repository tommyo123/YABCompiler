start tok64 matrix.prg
10 dim m(2, 3)
20 for i=0 to 2
30 for j=0 to 3
40 m(i, j) = i*10 + j
50 next j
60 next i
70 print "row major dump:"
80 for i=0 to 2
90 for j=0 to 3
100 print m(i, j);
110 next j
120 print
130 next i
140 print "diagonal:"
150 print m(0,0); m(1,1); m(2,2)
160 print "sum:"
170 t = 0
180 for i=0 to 2
190 for j=0 to 3
200 t = t + m(i, j)
210 next j
220 next i
230 print t
