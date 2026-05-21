start tok64 heap_gc_arr.prg
10 rem GC with string arrays
20 dim n$(5)
30 b$ = "hello"
40 f1 = fre(0)
50 for i=0 to 5
60 n$(i) = b$ + chr$(65 + i)
70 next i
80 for i=1 to 10
90 a$ = n$(0)
100 b$ = n$(1)
110 next i
120 f2 = fre(0)
130 print "leak:"; f1 - f2
140 print n$(0); " ";n$(1);" ";n$(2);" ";n$(3);" ";n$(4);" ";n$(5)
