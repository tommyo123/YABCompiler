10 PRINT ("ABC"="ABC");("ABC"<>"ABC");("ABC"="ABD")
20 PRINT ("ABC"<"ABD");("ABD">"ABC");("A"<"B")
30 PRINT ("ABC"<="ABC");("ABC">="ABC");("ABC"<="ABD")
40 PRINT ("AB"<"ABC");("ABCD">"ABC")
50 PRINT (""="");(""<"A");("A">"")
60 A$="HELLO":B$="WORLD"
70 PRINT (A$<B$);(A$>B$);(A$=A$)
80 IF A$<B$ THEN PRINT "H<W"
90 IF A$<>B$ THEN PRINT "DIFF"
100 IF "A"<"B" THEN PRINT "AB"
110 IF A$+"X"<>A$ THEN PRINT "GREW"
120 X$="A":Y$="B"
130 IF X$<Y$ THEN PRINT X$;"<";Y$
