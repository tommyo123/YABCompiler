start tok64 tab_spc.prg
10 print "col0"; tab(10); "col10"; tab(20); "col20"
20 print "a"; spc(5); "b"; spc(3); "c"
30 print tab(15); "indented"
40 print "x";
50 print tab(8); "y"
60 for i=1 to 3
70 print tab(i*5); "*"
80 next i
90 print "done"
