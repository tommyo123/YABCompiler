start tok64 heap_verify1.prg
10 rem test 1: print temp does not leak
20 f1 = fre(0)
30 for i=1 to 50
40 print "x" + "y";
50 next i
60 print
70 f2 = fre(0)
80 print "test1 leak:"; f1 - f2
