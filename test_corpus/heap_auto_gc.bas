start tok64 heap_auto_gc.prg
10 rem auto-GC test - no explicit fre call inside loop
20 c$ = "world"
30 f1 = fre(0): rem post-init free
40 for i=1 to 500
50 a$ = "hello" + c$
60 b$ = a$
70 next i
80 print "survived 500 iters"
90 print "free delta:"; f1 - fre(0)
100 print "len b$:"; len(b$); " val:"; b$
