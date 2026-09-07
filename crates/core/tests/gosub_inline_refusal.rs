//! `passes::gosub_inline_refusal` must stay welded to the inliners.

use yabcompiler_core::ast::{DataValue, FnName, VarKind, VarName};
use yabcompiler_core::ir::{DimSpec, Expr, Line, Module, Pass, Stmt, ThenIr};
use yabcompiler_core::passes::{GosubRefusal, GosubSingleUseInline, gosub_inline_refusal};

fn fvar(base: &str) -> VarName {
    VarName {
        base: base.to_string(),
        kind: VarKind::Float,
    }
}

/// A harmless statement that no gate objects to ; the filler every body needs so that "the body is empty" is never what is under test.
fn plain_let() -> Stmt {
    Stmt::Let {
        var: fvar("A"),
        value: Expr::Number(7.0),
    }
}

fn pass_inlines(body: &[Stmt]) -> bool {
    let mut module = Module {
        lines: vec![
            Line {
                number: 10,
                stmts: vec![Stmt::GoSub { target: 100 }, Stmt::Goto { target: 200 }],
            },
            Line {
                number: 100,
                stmts: body.to_vec(),
            },
            Line {
                number: 200,
                stmts: vec![Stmt::End],
            },
        ],
    };
    GosubSingleUseInline.run(&mut module).unwrap();
    // Inlined iff the `GOSUB` is gone from the caller.
    !module.lines[0]
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::GoSub { .. }))
}

fn table() -> Vec<(&'static str, Vec<Stmt>, Option<GosubRefusal>)> {
    vec![

        ("bare RETURN", vec![Stmt::Return], None),
        ("LET then RETURN", vec![plain_let(), Stmt::Return], None),
        (
            "POKE then RETURN",
            vec![
                Stmt::Poke {
                    addr: Expr::Number(53280.0),
                    value: Expr::Number(0.0),
                },
                Stmt::Return,
            ],
            None,
        ),
        (
            "IF with a safe THEN body, then RETURN",
            vec![
                Stmt::If {
                    cond: Expr::Var(fvar("A")),
                    then: ThenIr::Stmts(vec![plain_let()]),
                },
                Stmt::Return,
            ],
            None,
        ),

        ("empty line", vec![], Some(GosubRefusal::EmptyLine)),
        (
            "no trailing RETURN",
            vec![plain_let()],
            Some(GosubRefusal::NoTrailingReturn),
        ),
        (
            "GOTO in the body",
            vec![Stmt::Goto { target: 200 }, Stmt::Return],
            Some(GosubRefusal::Branch),
        ),
        (
            "IF ... THEN <line> in the body",
            vec![
                Stmt::If {
                    cond: Expr::Var(fvar("A")),
                    then: ThenIr::Goto(200),
                },
                Stmt::Return,
            ],
            Some(GosubRefusal::Branch),
        ),
        (
            "GOTO nested behind THEN",
            vec![
                Stmt::If {
                    cond: Expr::Var(fvar("A")),
                    then: ThenIr::Stmts(vec![Stmt::Goto { target: 200 }]),
                },
                Stmt::Return,
            ],
            Some(GosubRefusal::Branch),
        ),
        (
            "nested GOSUB",
            vec![Stmt::GoSub { target: 200 }, Stmt::Return],
            Some(GosubRefusal::NestedGosub),
        ),
        (
            "early RETURN",
            vec![Stmt::Return, Stmt::Return],
            Some(GosubRefusal::EarlyReturn),
        ),
        (
            "FOR opens a frame",
            vec![
                Stmt::For {
                    var: fvar("I"),
                    start: Expr::Number(1.0),
                    end: Expr::Number(3.0),
                    step: Expr::Number(1.0),
                    body_int_safe: true,
                    body_reads_loop_var: true,
                    induction_const: None,
                    array_inductions: Vec::new(),
                },
                Stmt::Return,
            ],
            Some(GosubRefusal::ForFrame),
        ),
        (
            "NEXT in the body",
            vec![Stmt::Next { vars: vec![None] }, Stmt::Return],
            Some(GosubRefusal::ForFrame),
        ),
        (
            "END in the body",
            vec![Stmt::End, Stmt::Return],
            Some(GosubRefusal::Terminator),
        ),
        (
            "STOP in the body",
            vec![Stmt::Stop, Stmt::Return],
            Some(GosubRefusal::Terminator),
        ),
        (
            "RUN in the body",
            vec![Stmt::Run(None), Stmt::Return],
            Some(GosubRefusal::Terminator),
        ),
        (
            "CLR in the body",
            vec![Stmt::Clr, Stmt::Return],
            Some(GosubRefusal::Clr),
        ),
        (
            "DATA in the body",
            vec![
                Stmt::Data(vec![DataValue::Float(1.0)]),
                Stmt::Return,
            ],
            Some(GosubRefusal::Declaration),
        ),
        (
            "DIM in the body",
            vec![
                Stmt::Dim(vec![DimSpec {
                    name: fvar("B"),
                    dims: vec![Expr::Number(10.0)],
                }]),
                Stmt::Return,
            ],
            Some(GosubRefusal::Declaration),
        ),
        (
            "DEF FN in the body",
            vec![
                Stmt::DefFn {
                    name: FnName("FA".to_string()),
                    param: fvar("X"),
                    body: Expr::Number(1.0),
                },
                Stmt::Return,
            ],
            Some(GosubRefusal::Declaration),
        ),
    ]
}

#[test]
fn refusal_is_none_exactly_when_the_pass_inlines() {
    let mut inlined_count = 0usize;
    let mut declined_count = 0usize;
    for (name, body, _) in table() {
        let refusal = gosub_inline_refusal(&body);
        let actually_inlined = pass_inlines(&body);
        assert_eq!(
            refusal.is_none(),
            actually_inlined,
            "{name}: gosub_inline_refusal said {refusal:?} but the pass \
             {} inline it",
            if actually_inlined { "DID" } else { "did NOT" }
        );
        if actually_inlined {
            inlined_count += 1;
        } else {
            declined_count += 1;
        }
    }
    // Non-vacuity: the table must exercise both answers.
    assert!(
        inlined_count >= 4,
        "table lost its positive counterweights: only {inlined_count} shapes inline"
    );
    assert!(
        declined_count >= 10,
        "table lost its refusals: only {declined_count} shapes decline"
    );
}

#[test]
fn each_refusal_reports_its_own_reason() {
    for (name, body, want) in table() {
        assert_eq!(
            gosub_inline_refusal(&body),
            want,
            "{name}: wrong refusal reason"
        );
    }
}

#[test]
fn slugs_are_stable_and_distinct() {
    let all = [
        (GosubRefusal::EmptyLine, "empty-line"),
        (GosubRefusal::NoTrailingReturn, "no-trailing-return"),
        (GosubRefusal::Branch, "branch"),
        (GosubRefusal::NestedGosub, "nested-gosub"),
        (GosubRefusal::EarlyReturn, "early-return"),
        (GosubRefusal::ForFrame, "for-frame"),
        (GosubRefusal::Terminator, "terminator"),
        (GosubRefusal::Clr, "clr"),
        (GosubRefusal::ErrorState, "error-state"),
        (GosubRefusal::Declaration, "declaration"),
    ];
    for (r, slug) in all {
        assert_eq!(r.slug(), slug, "slug for {r:?} changed");
    }
    let mut seen: Vec<&str> = all.iter().map(|(_, s)| *s).collect();
    seen.sort_unstable();
    let n = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), n, "two refusals share a slug");
}

#[test]
fn clr_prevents_gosub_inlining() {
    let body = vec![Stmt::Clr, Stmt::Return];
    assert_eq!(gosub_inline_refusal(&body), Some(GosubRefusal::Clr));
    assert!(
        !pass_inlines(&body),
        "CLR body was inlined; ?RETURN WITHOUT GOSUB would be lost"
    );

    assert!(pass_inlines(&[plain_let(), Stmt::Return]));
}

#[test]
fn mutation_notes() {}
