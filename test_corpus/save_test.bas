start tok64 save_test.prg
10 print "saving to disk..."
20 save "data.bin", 8
30 print "saved. status ="; st
40 print "verify..."
50 verify "data.bin", 8
60 print "verify status ="; st
70 print "done"
