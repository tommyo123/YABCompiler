start tok64 str_lex.prg
10 if "abc" < "abd" then print "abc<abd ok"
20 if "abc" < "abcd" then print "abc<abcd ok"
30 if "abcd" > "abc" then print "abcd>abc ok"
40 if "xyz" >= "xyz" then print "xyz>=xyz ok"
50 if "abc" <= "abc" then print "abc<=abc ok"
60 if "a" <> "b" then print "a<>b ok"
70 if "z" > "a" then print "z>a ok"
80 print "lex tests done"
