start tok64 heap_gc.prg
10 rem stress test for shared-var GC
20 b$ = "hello"
30 c$ = "world"
40 f1 = fre(0)
50 for i=1 to 50
60 a$ = b$ + c$
70 d$ = a$
80 e$ = d$
90 next i
100 f2 = fre(0)
110 print "leak:"; f1 - f2
120 print "len a$:"; len(a$); " len d$:"; len(d$); " len e$:"; len(e$)
