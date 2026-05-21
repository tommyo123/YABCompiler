start tok64 ti.prg
10 print "start ti ="; ti
20 print "ti$ at start:"; ti$
30 t = ti
40 for i=1 to 1000
50 next i
60 print "end ti ="; ti
70 print "elapsed ="; ti - t
80 print "in seconds:"; (ti - t) / 60
90 print "ti$ at end:"; ti$
