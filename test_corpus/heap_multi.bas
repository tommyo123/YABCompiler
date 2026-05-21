start tok64 heap_multi.prg
10 rem multi-var loop, runtime exprs that lastvar can't help across vars
20 b$ = "hello"
30 c$ = "world"
40 f1 = fre(0)
50 for i=1 to 50
60 a$ = left$(b$, 3)
70 d$ = right$(c$, 2)
80 e$ = b$ + c$
90 next i
100 f2 = fre(0)
110 print "leak:"; f1 - f2
