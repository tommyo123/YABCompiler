start tok64 heap_post_gc.prg
10 rem verify LASTVAR works after GC
20 b$ = "abc"
30 a$ = b$ + "x"
40 d = fre(0): rem triggers GC; a$ chunk may have moved
50 a$ = b$ + "y"
60 a$ = b$ + "z"
70 a$ = b$ + "w"
80 e = fre(0)
90 print "post-gc leak:"; d - e
100 print "a$:"; a$
