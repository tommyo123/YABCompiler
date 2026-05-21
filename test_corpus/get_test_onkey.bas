start tok-simon get_test_onkey.prg
5 on key "q" goto 100
10 print "press q"
20 a%=0
30 a%=a%+1
40 if a%>20000 then a%=0
50 goto 30
100 print "got q!"
110 disable
120 end
