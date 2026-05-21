start tok64 concat.prg
10 a$ = "hello"
20 b$ = "world"
30 c$ = a$ + " " + b$
40 print c$
50 d$ = a$ + chr$(33)
60 print d$
70 e$ = ""
80 for i=1 to 5
90 e$ = e$ + "*"
100 next i
110 print e$
