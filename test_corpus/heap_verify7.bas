start tok64 heap_verify7.prg
10 rem test 7: if cond with runtime concat (var-based)
20 b$ = "hello"
30 f1 = fre(0)
40 for i=1 to 50
50 if b$ + "x" = "hellox" then a = a + 1
60 next i
70 f2 = fre(0)
80 print "matches:"; a
90 print "leak:"; f1 - f2
