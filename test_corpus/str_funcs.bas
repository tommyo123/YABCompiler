start tok64 str_funcs.prg
10 a$ = "hello world"
20 print left$(a$, 5)
30 print right$(a$, 5)
40 print mid$(a$, 7, 5)
50 print mid$(a$, 7)
60 print str$(42)
70 print str$(-7)
80 b$ = "n=" + str$(100)
90 print b$
100 print left$(a$, 100)
110 print right$(a$, 0)
120 print mid$(a$, 200)
