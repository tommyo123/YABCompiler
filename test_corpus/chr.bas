start tok64 chr.prg
10 print chr$(147)
20 print "screen cleared"
30 print chr$(18);"reverse";chr$(146);" normal"
40 for i=1 to 5
50 print chr$(64+i);
60 next i
70 print
80 print "done"
