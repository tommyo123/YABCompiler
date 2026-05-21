start tok64 val_test.prg
10 a$ = "42"
20 b = val(a$)
30 print "val of 42 is"; b
40 c$ = "100"
50 d = val(c$)
60 print "val of 100 is"; d
70 e = val("7")
80 print "direct val 7 is"; e
90 print "decimal val is"; val("3.14")
100 print "exponent val is"; val("1e3")
110 print "usr default is"; usr(7)
