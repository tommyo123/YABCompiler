start tok64 pos_fre.prg
10 print "abc"; pos(0); "def"
20 print "free at start:"; fre(0)
30 a$ = "x" + "y"
40 print "free after concat:"; fre(0)
50 b$ = "abcdefghij"
60 c$ = a$ + b$
70 print "free after more:"; fre(0)
