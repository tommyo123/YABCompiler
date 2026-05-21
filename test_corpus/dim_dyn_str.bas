start tok64 dim_dyn_str.prg
10 n = 4
20 dim w$(n)
30 w$(0) = "alpha"
40 w$(1) = "beta"
50 w$(2) = "gamma"
60 w$(3) = "delta"
70 w$(4) = "epsilon"
80 for i = 0 to n
90 print i; ":"; w$(i)
100 next i
110 print "uninit slot reads as empty: ("; w$(0); ")"
