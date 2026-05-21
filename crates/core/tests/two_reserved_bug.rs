use asm6502::Assembler6502;

#[test]
fn two_reserved_ranges_with_branches_dense() {
    // Stress test: many short branches sprinkled throughout a long
    // code stream that crosses two reserved ranges. Each branch
    // targets a label far away (forcing long-form expansion). Models
    // maze eater's branch-heavy IF/FOR pattern.
    let mut asm = String::from("*=$0810\nstart:\n");
    let label_count = 50;
    // Forward branches that cross the reservations.
    for i in 0..label_count {
        asm.push_str(&format!("    BNE far_{i}\n"));
        for _ in 0..50 {
            asm.push_str("    NOP\n");
        }
    }
    // Bulk filler to push label positions past both reservations.
    for _ in 0..15000 {
        asm.push_str("    NOP\n");
    }
    for i in 0..label_count {
        asm.push_str(&format!("far_{i}:\n"));
        asm.push_str("    NOP\n");
    }

    let mut assembler = Assembler6502::new();
    assembler.add_reserved_range(0x3000, 0x34C0).unwrap();
    assembler.add_reserved_range(0x3800, 0x3FFF).unwrap();
    let result = assembler.assemble_bytes(&asm);
    assert!(result.is_ok(), "dense branches: {result:?}");
}
