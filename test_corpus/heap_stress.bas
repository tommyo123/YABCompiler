start tok64 heap_stress.prg
10 a$ = ""
20 f1 = fre(0)
30 for i=1 to 200
40 a$ = "x"
50 next i
60 f2 = fre(0)
70 print "200x reassign single char: leak ="; f1 - f2
80 a$ = ""
90 f1 = fre(0)
100 for i=1 to 100
110 a$ = a$ + "y"
120 if len(a$) > 20 then a$ = "z"
130 next i
140 f2 = fre(0)
150 print "build/reset 100 iters: leak ="; f1 - f2
160 print "final a$ len:"; len(a$)
