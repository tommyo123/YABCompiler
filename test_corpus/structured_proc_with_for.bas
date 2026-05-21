start tok-simon structured_proc_with_for.prg
10 t%=0
20 exec sumloop
30 print "total"; t%
40 end
50 proc sumloop
60 for i%=1 to 10
70 t%=t%+i%
80 next i%
90 end proc
