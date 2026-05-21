start tok64 int_arr.prg
10 dim a%(5), b%(3, 2)
20 for i = 0 to 5
30 a%(i) = i * i - 10
40 next i
50 for i = 0 to 5
60 print "a%("; i; ")="; a%(i)
70 next i
80 b%(2, 1) = -7
90 b%(0, 0) = 32000
100 print "b%(2,1)="; b%(2,1); " b%(0,0)="; b%(0,0)
110 print "sum:"; a%(0) + a%(1) + a%(2) + a%(3) + a%(4) + a%(5)
