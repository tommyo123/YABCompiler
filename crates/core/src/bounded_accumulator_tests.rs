use super::*;
use crate::ir::Pass;

fn module(src: &str) -> ir::Module {
    let bytes = crate::tokenize_program(src).unwrap();
    let program = crate::prg::Program::parse(&bytes).unwrap();
    let ast = crate::parse::program(&program).unwrap();
    let mut module = ir::lower(&ast).unwrap();
    ConstantFold.run(&mut module).unwrap();
    ResolveBareNext.run(&mut module).unwrap();
    module
}

fn s_range(src: &str) -> Option<PromRange> {
    bounded_accumulator_ranges(&module(src))
        .get(&VarName {
            base: "S".into(),
            kind: VarKind::Float,
        })
        .copied()
}

#[test]
fn split_family_identity_preserves_local_variable_mangling() {
    for (name, expected) in [
        ("S__3", "S"),
        ("PROC__A", "PROC__A"),
        ("PROC__B", "PROC__B"),
        ("PROC__A__2", "PROC__A"),
        ("__LICM_1", "__LICM_1"),
    ] {
        assert_eq!(
            split_family_base(&VarName {
                base: name.into(),
                kind: VarKind::Float
            }),
            expected
        );
    }
}

#[test]
fn float_and_percent_accumulator_families_remain_independent() {
    let m = module("10 S=0:S%=1000\n20 FOR I=1 TO 100\n30 S=S+1:S%=S%+200\n40 NEXT\n50 PRINT S;S%\n");
    let ranges = bounded_accumulator_ranges(&m);
    assert_eq!(ranges[&VarName { base: "S".into(), kind: VarKind::Float }], PromRange::new(0, 100));
    assert_eq!(ranges[&VarName { base: "S".into(), kind: VarKind::Integer }], PromRange::new(0, 21000));
}

#[test]
fn trip_bounds_multiply_nesting_and_sum_every_site() {
    assert_eq!(s_range("10 S=0\n20 FOR I=1 TO 10\n30 FOR J=20 TO 1 STEP -2\n40 S=S+1:S=S+2\n50 NEXT J\n60 NEXT I\n70 PRINT S\n"),
        Some(PromRange::new(0, 300)));
}

#[test]
fn opposite_steps_and_resets_only_widen_the_hull() {
    assert_eq!(
        s_range("10 S=100\n20 FOR I=1 TO 20\n30 S=S+3:S=S-2\n40 S=50\n50 NEXT\n60 PRINT S\n"),
        Some(PromRange::new(-40, 160))
    );
}

#[test]
fn reversed_bounds_execute_once_and_initial_zero_is_included() {
    assert_eq!(
        s_range("10 FOR I=10 TO 1\n20 S=S+1\n30 NEXT\n40 PRINT S\n"),
        Some(PromRange::new(0, 1))
    );
}

#[test]
fn split_lifetimes_keep_the_original_accumulator_bound() {
    let mut m =
        module("10 S=0\n20 FOR I=1 TO 100\n30 S=S+1:S=S+1:S=S+1:S=S+1\n40 NEXT\n50 PRINT S\n");
    SplitMultiTypeVars.run(&mut m).unwrap();
    let bounds = bounded_accumulator_ranges(&m);
    let family: Vec<_> = bounds
        .iter()
        .filter(|(v, _)| split_family_base(v) == "S")
        .collect();
    assert_eq!(
        family.len(),
        4,
        "exercise actual split lifetimes: {bounds:?}"
    );
    assert!(family.iter().all(|(_, r)| **r == PromRange::new(0, 400)));
}

#[test]
fn literal_overflow_is_not_mistaken_for_a_small_trip_bound() {
    assert_eq!(
        s_range("10 S=0\n20 FOR I=1 TO 200\n30 S=S+200\n40 NEXT\n50 PRINT S\n"),
        Some(PromRange::new(0, 40000))
    );
}

#[test]
fn variable_and_fractional_steps_have_no_trip_proof() {
    for step in ["A", "0.5", "S", "I"] {
        assert_eq!(
            s_range(&format!(
                "10 S=0:A=200\n20 FOR I=1 TO 200\n30 S=S+{step}\n40 NEXT\n50 PRINT S\n"
            )),
            None,
            "step {step}"
        );
    }
}

#[test]
fn counter_mutations_and_unmodelled_transfers_have_no_trip_proof() {
    for src in [
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:I=I-1\n40 NEXT\n50 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:IF S<20 THEN I=I-1\n40 NEXT\n50 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1\n40 NEXT\n50 IF S<40000 THEN 20\n60 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:IF S<40000 THEN 30\n40 NEXT\n50 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:SYS 828\n40 NEXT\n50 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:A=USR(0)\n40 NEXT\n50 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:A=FN X(0)\n40 NEXT\n50 PRINT S\n",
    ] {
        assert_eq!(s_range(src), None, "must refuse:\n{src}");
    }
}

#[test]
fn issue2_nonlexical_next_and_crossing_lifetimes_have_no_trip_proof() {
    for src in [
        "10 S=0:GOTO 30\n20 NEXT I:GOTO 60\n30 FOR I=1 TO 2\n40 S=S+1:GOTO 20\n60 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:GOTO 60\n40 NEXT I\n50 END\n60 NEXT I\n70 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 S=S+1:IF I=1 THEN 60\n40 NEXT\n50 PRINT S:END\n60 GOTO 30\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 FOR I=1 TO 2\n40 S=S+1\n50 NEXT I\n60 NEXT I\n70 PRINT S\n",
        "10 S=0\n20 FOR I=1 TO 2\n30 FOR J=1 TO 2\n40 S=S+1\n50 NEXT I\n60 PRINT S\n",
    ] {
        assert_eq!(s_range(src), None, "must refuse:\n{src}");
    }
}

#[test]
fn dynamic_fractional_or_zero_for_steps_have_no_trip_proof() {
    for header in [
        "I=1 TO N",
        "I=1 TO 10 STEP 0.5",
        "I=1 TO 10 STEP 0",
        "I=32767 TO 32767",
    ] {
        assert_eq!(
            s_range(&format!(
                "10 S=0:N=10\n20 FOR {header}\n30 S=S+1\n40 NEXT\n50 PRINT S\n"
            )),
            None,
            "header {header}"
        );
    }
}
