start tok64 data_read.prg
10 data 10, 20, 30, 40, 50
20 for i=1 to 5
30 read x
40 print x
50 next i
60 print "restoring..."
70 restore
80 read a, b, c
90 print a;b;c
100 data 100, 200
110 read x
120 print x
