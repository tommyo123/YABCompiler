start tok64 strings.prg
10 a$ = "hello"
20 b$ = "world"
30 print a$
40 print b$
50 c$ = a$
60 print c$
70 print "len of a$ is"; len(a$)
80 print "asc of a$ is"; asc(a$)
100 print "type q to quit, anything else echoes"
110 get k$
120 if k$="" then 110
130 if k$="q" then end
140 print k$
150 goto 110
