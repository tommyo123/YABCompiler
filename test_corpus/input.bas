start tok64 input.prg
10 input "your name"; n$
20 print "hello "; n$
30 input "age in years"; a
40 print n$; ", you are"; a; "years old"
50 print "in 10 years you'll be"; a+10
60 input "type 'q' to quit, anything else to loop: "; q$
70 if q$ <> "q" then 10
80 print "bye"
