start tok64 str_fold.prg
10 print "len:"; len("hello world")
20 print "asc:"; asc("a"); asc("z")
30 print "concat:"; "ab" + "cd" + "ef"
40 print "left:"; left$("hello", 3)
50 print "right:"; right$("hello", 3)
60 print "mid:"; mid$("hello", 2, 3)
70 print "mid open:"; mid$("hello", 3)
80 print "left clamp:"; left$("hi", 100)
90 a$ = "world"
100 print "runtime len:"; len(a$)
110 print "mixed:"; "x" + a$
