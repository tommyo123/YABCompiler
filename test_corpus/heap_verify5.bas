start tok64 heap_verify5.prg
10 rem test 5: leak in tight loop (LASTVAR not yet implemented - expected to leak)
20 f1 = fre(0)
30 a$ = ""
40 for i=1 to 30
50 a$ = a$ + "x"
60 next i
70 f2 = fre(0)
80 print "len a$ ="; len(a$)
90 print "expected leak (no LASTVAR yet):"; f1 - f2
