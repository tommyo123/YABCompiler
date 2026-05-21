start tok64 str_array.prg
10 dim n$(5)
20 n$(0) = "alice"
30 n$(1) = "bob"
40 n$(2) = "carol"
50 n$(3) = "david"
60 n$(4) = "eve"
70 for i=0 to 4
80 print i; n$(i)
90 next i
100 print "joined: "; n$(0) + " and " + n$(1)
110 print left$(n$(2), 3)
