start tok64 data_str.prg
10 data 42, "alice", 7, "bob", -3, "carol"
20 for i=1 to 3
30 read n, name$
40 print i; ":"; n; name$
50 next i
60 restore
70 read first
80 print "after restore, first ="; first
