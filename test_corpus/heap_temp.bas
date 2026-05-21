start tok64 heap_temp.prg
10 f1 = fre(0)
20 for i=1 to 100
30 a = len("x" + "y" + "z" + "abc")
40 next i
50 f2 = fre(0)
60 print "before:"; f1
70 print "after:"; f2
80 print "diff:"; f1 - f2
