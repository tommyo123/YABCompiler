//! Check cached subexpressions against exact interpreter results, including rounding.

mod common;

use yabcompiler_core::{CompileOptions, Profile, compile_with_options, tokenize_program};

/// Print the sign, mantissa halves and exponent without losing bits.
const EXACT: &str = "1000 IF V=0 THEN PRINT \"0 \";:RETURN\n\
                     1010 S=SGN(V):V=ABS(V):E=0\n\
                     1020 IF V>=1 THEN V=V/2:E=E+1:GOTO 1020\n\
                     1030 IF V<.5 THEN V=V*2:E=E-1:GOTO 1030\n\
                     1040 H=INT(V*65536):L=(V*65536-H)*65536:PRINT S*H;L;E;:RETURN\n";

const PROGRAMS: &[&str] = &[
    // Reuse rounded and unrounded operands; invalidate them when X changes.
    "10 FOR I=1 TO 20:X=I/7:Y=I*1.3-2\n\
     20 A=(X-Y)*(X-Y)+X*2:B=(X-Y)/3+SQR(ABS(X*Y+1)):C=SQR(ABS(X*Y+1))*(X-Y)\n\
     30 X=X+1:D=(X-Y)*2:E=SQR(ABS(X*Y+1))\n\
     40 V=A:GOSUB1000:V=B:GOSUB1000:V=C:GOSUB1000:V=D:GOSUB1000:V=E:GOSUB1000:PRINT\n\
     50 NEXT:END\n",
    // Assignments inside the run end some reuses and keep others.
    "10 FOR I=1 TO 20:X=I/7:Y=I*1.3-2:A=(X-Y)*(X-Y):X=X+.5:B=(X-Y)*(X-Y):Y=A*B:C=(X-Y)*(X-Y)+A*B\
     :V=A:GOSUB1000:V=B:GOSUB1000:V=C:GOSUB1000:V=Y:GOSUB1000:PRINT:NEXT\n\
     20 END\n",
    // An array element written between two uses.
    "10 DIM A(5):FOR I=0 TO 5:A(I)=I*1.1+.3:NEXT\n\
     20 FOR I=1 TO 4:P=A(I)*A(I-1)+A(I+1)/3:A(I)=A(I)*A(I-1)+1:Q=A(I)*A(I-1)+A(I+1)/3\
     :V=P:GOSUB1000:V=Q:GOSUB1000:PRINT:NEXT\n\
     30 END\n",
    // Integer and comparison uses of a reused value.
    "10 FOR I=1 TO 30:X=I*.37:Y=X*X-I:Z=(X*X-I)*2+(X*X-I):W=1/(X*X-I+.5):V=INT(X*X-I):U=-(X*X-I>3)\
     :V=Y:GOSUB1000:V=Z:GOSUB1000:V=W:GOSUB1000:V=V:GOSUB1000:V=U:GOSUB1000:PRINT:NEXT\n\
     20 END\n",
    // Divisions, SQR and EXP results reused, one into an integer variable.
    "10 FOR I=1 TO 25:X=I/3:Y=7/I\n\
     20 A=(X/3)*Y:B=Y*(X/3):C=SQR(X*X+Y*Y):D=SQR(X*X+Y*Y)+X*X:E=EXP(X/9)*EXP(X/9)-(X/3)/(X/3)\n\
     30 F%=(X/3)*10:G=(X/3)*10:V=A:GOSUB1000:V=B:GOSUB1000:V=C:GOSUB1000:V=D:GOSUB1000:V=E:GOSUB1000\
     :V=F%:GOSUB1000:V=G:GOSUB1000:PRINT\n\
     40 NEXT:END\n",
    // Repeated array expressions in a quadratic calculation.
    "10 K=-1:T=99999:AK=2:DIM KX(3),KY(3),KR(3):KX(0)=3:KY(0)=9:KR(0)=9:KX(1)=-12:KY(1)=7:KR(1)=7\
     :KX(2)=-3:KY(2)=3:KR(2)=3\n\
     20 OX=.5:OY=1.25:RX=.3:RY=-.7\n\
     30 FORI=0TOAK:ZW=RX*RX+RY*RY:P=(2*(RX*(OX-KX(I))+RY*(OY-KY(I))))/ZW\
     :Q=((OX-KX(I))*(OX-KX(I))+(OY-KY(I))*(OY-KY(I))-KR(I)*KR(I))/ZW:Z1=-P/2:Z2=((P*P)/4-Q)\
     :V=P:GOSUB1000:V=Q:GOSUB1000:V=Z1:GOSUB1000:V=Z2:GOSUB1000:PRINT:NEXT\n\
     40 END\n",
    // Array elements with a float index, the index and the array changed in between.
    "10 DIM KX(3),KY(3):FOR I=0 TO 3:KX(I)=I*3-4:KY(I)=I*I+.5:NEXT\n\
     20 FOR J=0 TO 6:K=J/2.7:RX=J*.31+.1:RY=.7-J*.05\n\
     30 X1=KX(K)*RX+KY(K)*RY:F1=KX(K)-RX:F2=KY(K)*KY(K):K=K+.4:F3=KX(K)+F1:KX(1)=KX(K)*2\
     :F4=KX(1)*KX(K)\n\
     40 V=X1:GOSUB1000:V=F1:GOSUB1000:V=F2:GOSUB1000:V=F3:GOSUB1000:V=F4:GOSUB1000:PRINT:NEXT\n\
     50 END\n",
];

type Outcome = (String, Option<u8>, Option<u16>);

fn interpreted(src: &str) -> Outcome {
    let prg = tokenize_program(src).expect("tokenizes");
    let r = emu64::run_basic_to_end(&prg, 400_000_000).expect("runs in the interpreter");
    (
        common::petscii_to_string(&r.output),
        r.error_code,
        r.error_line,
    )
}

fn compiled(src: &str, profile: Profile, fast_float: bool) -> (Outcome, String) {
    let prg = tokenize_program(src).expect("tokenizes");
    let c = compile_with_options(
        &prg,
        CompileOptions {
            profile,
            fast_float,
            ..CompileOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiles ({profile:?}): {e}\n{src}"));
    let r = emu64::run_prg_to_end(&c.prg_bytes, 400_000_000).expect("runs");
    (
        (
            common::petscii_to_string(&r.output),
            r.error_code,
            r.error_line,
        ),
        c.asm,
    )
}

#[test]
fn reused_subexpressions_match_the_interpreter_to_the_bit() {
    for body in PROGRAMS {
        let src = format!("{body}{EXACT}");
        let want = interpreted(&src);
        assert_eq!(want.1, None, "the program itself runs clean:\n{src}");
        for (profile, fast_float) in [
            (Profile::Default, false),
            (Profile::Speed, false),
            (Profile::Size, false),
            (Profile::Default, true),
            (Profile::Speed, true),
        ] {
            let (got, asm) = compiled(&src, profile, fast_float);
            assert_eq!(got, want, "{profile:?} fast float {fast_float}:\n{src}");
            assert_eq!(
                asm.contains("__CSE_SLOTS"),
                profile != Profile::Size,
                "{profile:?}: only the size profile recomputes:\n{src}"
            );
        }
    }
}

/// Invalidate cached expressions after scalar or array writes.
#[test]
fn an_assignment_in_between_is_seen() {
    let src = "10 X=3:Y=.25:A=(X-Y)*(X-Y):X=X+1:B=(X-Y)*(X-Y):PRINT A;B\n\
               20 DIM K(2):K(1)=7:P=K(1)*1.5+K(1):K(1)=2:Q=K(1)*1.5+K(1):PRINT P;Q\n";
    let want = interpreted(src);
    for profile in [Profile::Default, Profile::Speed] {
        let (got, _) = compiled(src, profile, false);
        assert_eq!(got, want, "{profile:?}");
    }
}
