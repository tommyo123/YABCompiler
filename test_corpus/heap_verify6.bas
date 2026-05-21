start tok64 heap_verify6.prg
10 rem test 6: if-with-cond using concat does not leak
20 f1 = fre(0)
30 for i=1 to 50
40 if "a" + "b" = "ab" then a = a + 1
50 next i
60 f2 = fre(0)
70 print "matches:"; a
80 print "leak:"; f1 - f2
