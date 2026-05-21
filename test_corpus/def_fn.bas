start tok64 def_fn.prg
10 def fn sq(x) = x*x
20 def fn lin(x) = 2*x + 3
30 print "sq(7) ="; fn sq(7)
40 print "sq(-4) ="; fn sq(-4)
50 print "lin(5) ="; fn lin(5)
60 print "lin(0) ="; fn lin(0)
70 print "compose: sq(lin(2)) ="; fn sq(fn lin(2))
80 for i=0 to 4
90 print i; "->"; fn sq(i)
100 next i
110 a = fn lin(10)
120 print "a now ="; a
