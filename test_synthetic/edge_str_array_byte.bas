10 REM LEN/ASC on string-array elements
20 DIM A$(5)
30 A$(0)="HELLO":A$(1)="HI":A$(2)="ABCDE":A$(3)="X":A$(4)=""
40 FOR I=0 TO 4
50 PRINT I;LEN(A$(I));
60 IF LEN(A$(I))>0 THEN PRINT ASC(A$(I)):GOTO 75
65 PRINT "(empty)"
75 REM continue

70 NEXT
80 REM CHR$ of ASC of array element
90 PRINT CHR$(ASC(A$(0)));CHR$(ASC(A$(1)))
