10 REM String slicing opts: LEFT$/RIGHT$/MID$ all hit opt 1
20 REM (direct heap producer with non-allocating source).
30 S$="ABCDEFGHIJ"
40 A$=LEFT$(S$,3)
50 B$=RIGHT$(S$,3)
60 C$=MID$(S$,4,3)
70 PRINT A$;"-";B$;"-";C$
80 REM Now chain — LEFT$ of a Concat (Concat is allocating, so
90 REM opt 1 should NOT fire, falls back to general path).
100 D$=LEFT$(S$+"_TAIL",5)
110 PRINT D$
