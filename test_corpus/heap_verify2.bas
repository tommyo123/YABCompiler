start tok64 heap_verify2.prg
10 rem test 2: letstr keeps its chunk (no rollback)
20 a$ = "hello" + "world"
30 print "a$ ="; a$
40 b$ = "foo" + "bar"
50 print "a$ still ="; a$
60 print "b$ ="; b$
