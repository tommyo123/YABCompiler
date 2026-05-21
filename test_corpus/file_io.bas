start tok64 file_io.prg
10 print "before open"
20 open 1, 3
30 print#1, "via channel 1"
40 print#1, "second line";
50 print#1, " continued"
60 close 1
70 print "after close"
