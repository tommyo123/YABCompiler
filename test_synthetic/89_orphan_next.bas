10 goto 30
20 next:end
30 for i=0 to 10
40 print i;
50 gosub 100
60 goto 20
100 for p=0 to 40
110 print tab(1),p
120 if p=4 then return
130 goto 20