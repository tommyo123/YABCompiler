start tok64 wait_test.prg
10 poke 49152, 5
20 print "before wait1"
30 wait 49152, 4
40 print "after wait1"
50 poke 49152, 0
60 print "before wait2"
70 wait 49152, 1, 1
80 print "after wait2 (eor)"
