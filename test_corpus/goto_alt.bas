start tok64 goto_alt.prg
10 print "start"
20 go to 50
30 print "skipped"
40 end
50 print "via go to"
60 go sub 100
70 print "via go sub"
80 end
100 print "in subroutine"
110 return
