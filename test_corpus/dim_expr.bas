start tok64 dim_expr.prg
10 dim a(5+3), b(2*4, 3+2)
20 a(7) = 100
30 b(7, 4) = 200
40 print "a(7)="; a(7); "b(7,4)="; b(7,4)
50 dim c(len("hello"))
60 c(5) = 999
70 print "c(5)="; c(5)
