start tok64 int_inline.prg
10 a% = 5
20 b% = -3
30 c% = a%
40 print "init a%,b%,c%:"; a%; b%; c%
50 a% = a% + 1
60 b% = b% - 1
70 print "after inc/dec:"; a%; b%
80 c% = a% + b%
90 print "a%+b% via inc:"; c%
100 c% = a% - b%
110 print "a%-b%:"; c%
120 d% = 100
130 e% = 200
140 c% = d% + e%
150 print "100+200:"; c%
160 c% = d% - e%
170 print "100-200:"; c%
180 c% = a% + 10
190 print "a%+10:"; c%
200 c% = -5 + a%
210 print "-5+a%:"; c%
220 t% = 0
230 for i% = 1 to 100
240 t% = t% + 1
250 next i%
260 print "loop count:"; t%
300 a% = 5: b% = 7
310 if a% < b% then print "5<7 ok"
320 if a% > b% then print "5>7 BAD" : goto 330
325 print "5>7 ok"
330 if a% = 5 then print "a%=5 ok"
340 if a% <> b% then print "5<>7 ok"
350 if a% <= 5 then print "5<=5 ok"
360 if a% >= 5 then print "5>=5 ok"
370 if b% <= a% then print "7<=5 BAD" : goto 380
375 print "7<=5 ok"
380 c% = -1000: d% = 1000
390 if c% < d% then print "-1000<1000 ok"
400 if c% < 0 then print "neg ok"
410 if d% > 0 then print "pos ok"
420 e% = -32768
430 if e% < 0 then print "min ok"
440 print "if-tests done"
