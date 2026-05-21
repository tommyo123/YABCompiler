start tok64 status_test.prg
10 print "initial st ="; st
20 close 1
30 print "after close 1 ="; st
40 open 1, 3
50 print "after open 1,3 ="; st
60 print#1, "hello via channel"
70 close 1
80 print "after close ok ="; st
