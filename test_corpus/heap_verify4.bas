start tok64 heap_verify4.prg
10 rem test 4: multi-stmt line - mixed let and print
20 f1 = fre(0)
30 a$ = "x" + "y" : print "got " + a$ : a$ = "p" + "q" : print a$
40 f2 = fre(0)
50 print "diff:"; f1 - f2
