start tok64 dim_dyn_oom.prg
10 print "trying dim that overflows free heap"
20 n = 10000
30 dim a(n)
40 print "should not reach"
