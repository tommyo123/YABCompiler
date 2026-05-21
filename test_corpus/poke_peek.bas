start tok64 poke_peek.prg
10 poke 1024,1
20 poke 1025,2
30 poke 1026,3
40 print peek(1024)
50 print peek(1025)
60 print peek(1026)
70 for i=0 to 9
80 poke 1280+i,65+i
90 next i
100 print "border:"
110 poke 53280,7
120 print peek(53280)
