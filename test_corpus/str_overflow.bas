start tok64 str_overflow.prg
10 a$ = "1234567890123456789012345678901234567890"
20 b$ = a$ + a$
30 c$ = b$ + b$
40 d$ = c$ + c$
50 print "len d$ ="; len(d$)
60 e$ = d$ + d$
70 print "should not reach"
