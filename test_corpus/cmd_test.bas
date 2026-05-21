start tok64 cmd_test.prg
10 print "before cmd (screen)"
20 open 1, 3
30 cmd 1, "redirected"
40 print "this also via cmd"
50 print "and this"
60 close 1
70 print "after close (back to screen)"
