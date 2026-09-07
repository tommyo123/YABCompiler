//! Remove float mirrors only when no emitted instruction references them.

use std::collections::HashSet;

use crate::ast::VarName;

/// At most two speculative emissions, then an unconditional conservative emission.
pub(super) fn emit_verified<T, E>(
    mut omitted: HashSet<VarName>,
    mut emit: impl FnMut(&HashSet<VarName>) -> Result<(String, T), E>,
) -> Result<(String, T), E> {
    for _ in 0..2 {
        let output = match emit(&omitted) {
            Ok(output) => output,
            // Speculation may not introduce a new compile refusal either.
            Err(_) if !omitted.is_empty() => return emit(&HashSet::new()),
            Err(error) => return Err(error),
        };
        let referenced = referenced_slots(&output.0, &omitted);
        if referenced.is_empty() {
            return Ok(output);
        }
        omitted.retain(|var| !referenced.contains(var));
    }
    emit(&HashSet::new())
}

fn referenced_slots(asm: &str, omitted: &HashSet<VarName>) -> HashSet<VarName> {
    if omitted.is_empty() {
        return HashSet::new();
    }

    let tokens: HashSet<&str> = asm
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|token| token.starts_with("V_"))
        .collect();
    omitted
        .iter()
        .filter(|var| tokens.contains(var.label().as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::VarKind;

    fn var(name: &str) -> VarName {
        VarName {
            base: name.into(),
            kind: VarKind::Float,
        }
    }

    #[test]
    fn all_symbol_forms_restore_the_mirror() {
        let x = var("X");
        let candidates = HashSet::from([x.clone()]);
        for text in [
            "LDA V_X",
            "LDA V_X+4",
            "LDA #<V_X",
            "LDY #>V_X",
            ".word V_X",
            "ALIAS = V_X+2",
            "__LD_X:\nLDA #<V_X\nJMP $BBA2",
        ] {
            assert_eq!(referenced_slots(text, &candidates), candidates, "{text}");
        }
        assert!(referenced_slots("ZSI_V_X = $02\nLDA V_X1\nSTA V_X__1", &candidates).is_empty());
    }

    #[test]
    fn planted_missing_reader_is_regenerated_with_its_mirror() {
        let candidates = HashSet::from([var("X"), var("Y")]);
        let mut calls = 0;
        let (asm, ()) = emit_verified(candidates, |omitted| {
            calls += 1;
            let text = if omitted.contains(&var("X")) {
                "LDA #<V_X" // missed FAC memory operand
            } else {
                assert!(omitted.contains(&var("Y"))); // retain independent win
                "V_X:\n.byte 0,0,0,0,0\nLDA #<V_X"
            };
            Ok::<_, ()>((text.to_owned(), ()))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert!(asm.starts_with("V_X:"));
    }

    #[test]
    fn a_reader_revealed_by_retry_falls_back_with_a_fixed_emission_bound() {
        let candidates = HashSet::from([var("X"), var("Y")]);
        let mut calls = 0;
        let (_, ()) = emit_verified(candidates, |omitted| {
            calls += 1;
            let text = if omitted.contains(&var("X")) {
                "LDA #<V_X"
            } else if omitted.contains(&var("Y")) {
                "LDY #>V_Y"
            } else {
                "V_X:\nV_Y:\nLDA #<V_X\nLDY #>V_Y"
            };
            Ok::<_, ()>((text.to_owned(), ()))
        })
        .unwrap();
        assert_eq!(calls, 3);
    }
}
