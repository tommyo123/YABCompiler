start tok64 for_safety.prg
10 print "case 1: clean body"
20 for i=1 to 5
30 print i;
40 next i
50 print
60 print "case 2: gosub in body (forces float)"
70 for i=1 to 3
80 gosub 1000
90 next i
100 print
110 print "case 3: write to loop var (forces float)"
120 for i=1 to 5
130 i = i + 1
140 next i
150 print "i ="; i
160 print "case 4: gosub-free read of var"
170 t = 0
180 for i=1 to 4
190 t = t + i*i
200 next i
210 print "sum of squares ="; t
220 end
1000 print "*";
1010 return
