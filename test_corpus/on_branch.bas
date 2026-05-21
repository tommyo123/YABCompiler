start tok64 on_branch.prg
10 for i=0 to 5
20 on i goto 100, 200, 300
30 print i; "fell through"
40 next i
50 for i=1 to 3
60 on i gosub 500, 600, 700
70 next i
80 print "done"
90 end
100 print i; "is one"
110 goto 40
200 print i; "is two"
210 goto 40
300 print i; "is three"
310 goto 40
500 print "sub a"
510 return
600 print "sub b"
610 return
700 print "sub c"
710 return
