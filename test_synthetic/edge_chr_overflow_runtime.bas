10 REM CHR$ where range can't be proven byte-safe
20 REM falls back to FAC + FAC_BYTE — value 65 fits at runtime
30 A=65.0
40 PRINT CHR$(A)
50 REM CHR$ with mixed float arith
60 X=33+RND(0)*0
70 PRINT CHR$(X)
80 REM Many CHR$ in chain to test invalidate_fac_cache
90 PRINT CHR$(72);CHR$(73);CHR$(74);CHR$(75);CHR$(76);CHR$(77);CHR$(78)
