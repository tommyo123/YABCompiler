start tok64 heap_alias.prg
10 rem aliased vars: lastvar disabled, p4 doesn't apply (shared)
20 b$ = "hello"
30 f1 = fre(0)
40 for i=1 to 30
50 a$ = b$ + chr$(65)
60 b$ = a$
70 next i
80 f2 = fre(0)
90 print "leak:"; f1 - f2
100 print "len a$:"; len(a$)
110 print "len b$:"; len(b$)
