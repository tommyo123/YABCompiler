start tok64 full_test.prg
10 print "counting:"
20 for i=1 to 10
30 if i=5 then gosub 100
40 print i
50 next i
60 print "sum 1+2+3:"
70 a=1+2+3
80 print a
90 end
100 print "halfway!"
110 return
