//! Share code regions with matching control flow and renamed internal labels.
//! Reject calls, hardware-stack access, and bank writes within a region.
use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone)]
struct Region {
    start: usize,
    end: usize,
    labels: Vec<String>,
    exit: String,
    bytes: usize,
    signature: String,
}

#[derive(Clone, Copy)]
struct Reference {
    at: usize,
    branch: bool,
    control: bool,
}

fn references(items: &[Item]) -> HashMap<String, Vec<Reference>> {
    let mut refs: HashMap<String, Vec<Reference>> = items
        .iter()
        .filter_map(|item| {
            if let Item::Label(name) = item {
                Some((name.clone(), Vec::new()))
            } else {
                None
            }
        })
        .collect();
    for (at, item) in items.iter().enumerate() {
        let (text, branch, control) = match item {
            Item::Insn(insn) => {
                let Some(op) = &insn.operand else { continue };
                let branch = is_cond_branch(&insn.mnem) || insn.mnem == "JMP";
                (op, branch, branch || insn.mnem == "JSR")
            }
            Item::Directive(raw) | Item::Origin(raw) => (raw, false, false),
            _ => continue,
        };
        for name in extract_labels_from_operand(text) {
            if let Some(users) = refs.get_mut(&name) {
                users.push(Reference {
                    at,
                    branch: branch && text == &name,
                    control: control && text == &name,
                });
            }
        }
    }
    refs
}

fn entry_is_private(items: &[Item], start: usize, refs: &HashMap<String, Vec<Reference>>) -> bool {
    let mut at = start;
    // A window can start after the first instruction of a basic block.
    while let Some(prev) = prev_struct(items, at) {
        if !matches!(&items[prev], Item::Insn(_)) {
            break;
        }
        at = prev;
    }
    while let Some(prev) = prev_struct(items, at) {
        let Item::Label(label) = &items[prev] else {
            break;
        };
        // Entry labels remain at the call site.
        if refs[label].iter().any(|r| !r.control) {
            return false;
        }
        at = prev;
    }
    true
}

pub(super) fn is_integer_or_string_home(name: &str) -> bool {
    ["VI_", "VS_"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
    })
}

struct Operands {
    equates: HashMap<String, Option<u16>>,
    homes: HashSet<String>,
}

impl Operands {
    fn scan(items: &[Item]) -> Self {
        let equates: HashMap<_, _> = items
            .iter()
            .filter_map(|item| {
                let Item::Directive(raw) = item else {
                    return None;
                };
                let (name, value) = raw.split_once('=')?;
                Some((
                    name.trim().to_string(),
                    parse_plain_addr(value.split(';').next().unwrap_or("").trim()),
                ))
            })
            .collect();
        let mut homes: HashSet<_> = equates
            .iter()
            .filter_map(|(name, value)| {
                (is_integer_or_string_home(name) && value.is_some()).then_some(name.clone())
            })
            .collect();
        // A code label with a familiar prefix is not a variable.
        let origins: Vec<_> = items
            .iter()
            .filter_map(|item| {
                if let Item::Origin(raw) = item {
                    Some(raw)
                } else {
                    None
                }
            })
            .collect();
        let data_above_stack = !origins.is_empty()
            && origins.iter().all(|raw| {
                raw.trim()
                    .strip_prefix("*=")
                    .and_then(|text| parse_plain_addr(text.trim()))
                    .is_some_and(|origin| origin >= 0x0200)
            });
        if data_above_stack {
            for (idx, item) in items.iter().enumerate() {
                let Item::Label(name) = item else { continue };
                if !is_integer_or_string_home(name) {
                    continue;
                }
                let mut at = idx;
                while let Some(next) = next_struct(items, at) {
                    match &items[next] {
                        Item::Label(_) => at = next,
                        Item::Directive(raw)
                            if raw.trim_start().starts_with(".byte ")
                                || raw.trim_start().starts_with(".word ") =>
                        {
                            homes.insert(name.clone());
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        Self { equates, homes }
    }
}

fn plain_instruction(insn: &Insn, operands: &Operands) -> bool {
    plain_instruction_with_policy(insn, operands, JsrPolicy::CompactRefused)
}

fn plain_instruction_with_policy(insn: &Insn, operands: &Operands, policy: JsrPolicy) -> bool {
    if matches!(
        insn.mnem.as_str(),
        "JSR" | "PHA" | "PLA" | "PHP" | "PLP" | "TSX" | "TXS" | "RTS" | "RTI" | "BRK"
    ) {
        return false;
    }
    if is_cond_branch(&insn.mnem) || insn.mnem == "JMP" {
        return insn
            .operand
            .as_deref()
            .is_some_and(|op| !op.starts_with('('));
    }
    if !is_factorable_insn_with_policy(insn, policy) {
        return false;
    }
    if insn.operand.as_deref().is_some_and(|op| {
        extract_labels_from_operand(op)
            .iter()
            .any(|name| is_integer_or_string_home(name) && !operands.homes.contains(name))
    }) {
        return false;
    }
    if let Some(op) = insn.operand.as_deref().filter(|op| !op.starts_with('#')) {
        // An indirect pointer might address the extra return frame.
        if op.starts_with('(') {
            return false;
        }
        let indexed = op.contains(',');
        let base = op.split(',').next().unwrap();
        let (name, offset) = match base.split_once('+') {
            Some((name, off)) => (name, parse_plain_addr(off)),
            None => (base, Some(0)),
        };
        if offset.is_none() || base.contains('-') {
            return false;
        }

        if is_integer_or_string_home(name) && (indexed || offset.unwrap() > 1) {
            return false;
        }
        let address = if let Some(value) = operands.equates.get(name) {
            let Some(value) = value else { return false };
            Some(*value)
        } else {
            parse_plain_addr(name)
        };
        if let Some(address) = address {
            let Some(first) = address.checked_add(offset.unwrap()) else {
                return false;
            };

            let last = u32::from(first) + if indexed { 255 } else { 0 };
            if first <= 0x01FF && last >= 0x0100 {
                return false;
            }
            if (first <= 1 || last > 0xFFFF)
                && matches!(
                    insn.mnem.as_str(),
                    "STA" | "STX" | "STY" | "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR"
                )
            {
                return false;
            }
            if indexed && first < 0x0100 {
                return false;
            }
        }
    }
    // Never move a bank/DDR write into a callable helper.
    if matches!(
        insn.mnem.as_str(),
        "STA" | "STX" | "STY" | "INC" | "DEC" | "ASL" | "LSR" | "ROL" | "ROR"
    ) && insn
        .operand
        .as_deref()
        .is_some_and(|op| matches!(op, "$00" | "$01" | "$0000" | "$0001"))
    {
        return false;
    }
    true
}

pub(super) fn compact_instruction_sites(items: &[Item]) -> Vec<bool> {
    let operands = Operands::scan(items);
    let private = private_instruction_sites(items);
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            matches!(item, Item::Insn(insn)
            if private[idx] && !is_cond_branch(&insn.mnem) && insn.mnem != "JMP"
                && plain_instruction(insn, &operands))
        })
        .collect()
}

/// Check that a local leaf call can move across the added call boundary.
pub(super) fn compact_leaf_call_sites(items: &[Item]) -> Vec<bool> {
    let reserved_contract = |name: &str| [
        "__SYS", "__FOR", "__GOSUB", "__RETURN", "__ONKEY", "__ON_KEY",
        "__JT", "__EXRAM", "__USR", "__STR_PRINT",
    ].iter().any(|prefix| name.starts_with(prefix));
    let mut sites = vec![false; items.len()];
    let private = private_instruction_sites(items);
    let operands = Operands::scan(items);
    let patched = smc_patched_labels(items);
    let mut leaves = HashSet::new();
    for (idx, item) in items.iter().enumerate() {
        let Item::Label(name) = item else { continue };
        if !name.strip_prefix("__SEQ_").is_some_and(|id|
            !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())) || reserved_contract(name) {
            continue;
        }
        let mut alias_at = idx;
        let mut sensitive_alias = false;
        while let Some(prev) = prev_struct(items, alias_at) {
            let Item::Label(alias) = &items[prev] else { break };
            sensitive_alias |= reserved_contract(alias);
            alias_at = prev;
        }
        if sensitive_alias { continue; }
        let mut at = idx;
        let mut count = 0;
        while let Some(next) = next_struct(items, at) {
            if crosses_sys_wrap(items, at, next) { break; }
            match &items[next] {

                Item::Label(alias) if count == 0 && !reserved_contract(alias) => {
                    at = next; continue;
                }
                Item::Insn(insn) if private[next] && !insn_is_smc_target(items, next, &patched) => {
                    if insn.mnem == "RTS" {
                        leaves.insert(name.clone());
                        break;
                    }
                    if count == 16 || is_cond_branch(&insn.mnem) || insn.mnem == "JMP"
                        || !plain_instruction_with_policy(insn, &operands, JsrPolicy::CompactLeafCalls) { break; }

                    if insn.operand.as_deref().is_some_and(|op| !op.starts_with('#')
                        && extract_labels_from_operand(op).iter()
                            .any(|name| !operands.homes.contains(name)
                                || operands.equates.get(name).is_some_and(|address|
                                    address.is_none_or(|address| address >= 0x0100)))) { break; }
                    count += 1;
                    at = next;
                }
                _ => break,
            }
        }
    }
    for (idx, item) in items.iter().enumerate() {
        if let Item::Insn(insn) = item {
            if insn.mnem == "JSR" && private[idx]
                && insn.operand.as_ref().is_some_and(|target| leaves.contains(target)) {
                sites[idx] = true;
            } else if private[idx] && !is_cond_branch(&insn.mnem) && insn.mnem != "JMP"
                && plain_instruction_with_policy(insn, &operands, JsrPolicy::CompactLeafCalls) {
                sites[idx] = true;
            }
        }
    }
    sites
}

/// Instructions outside SYS markers and address-visible code blocks.
pub(super) fn private_instruction_sites(items: &[Item]) -> Vec<bool> {
    let refs = references(items);
    let mut wrapped = false;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            if let Item::Comment(raw) = item {
                if raw.trim() == "; YAB_SYS_WRAP_IN" {
                    wrapped = true;
                }
                if raw.trim() == "; YAB_SYS_WRAP_OUT" {
                    wrapped = false;
                }
            }
            matches!(item, Item::Insn(_)) && !wrapped && entry_is_private(items, idx, &refs)
        })
        .collect()
}

fn collect(items: &[Item]) -> Vec<Region> {
    let operands = Operands::scan(items);
    let refs = references(items);
    let private = private_instruction_sites(items);
    let smc = smc_patched_labels(items);
    let zp = ZpEquates::scan(items);
    let mut regions = Vec::new();
    for start in 0..items.len() {
        if !private[start] {
            continue;
        }
        let mut labels: Vec<String> = Vec::new();
        let mut instructions = 0;
        let mut bytes = 0;
        let mut branched = false;
        for end in start..items.len() {
            if instructions > 40 || labels.len() > 10 {
                break;
            }
            match &items[end] {
                Item::Blank => {}
                Item::Comment(raw) => {
                    if raw.contains("YAB_SYS_WRAP") {
                        break;
                    }
                }
                Item::Directive(_) | Item::Origin(_) => break,
                Item::Insn(insn) => {
                    if !plain_instruction(insn, &operands) || insn_is_smc_target(items, end, &smc) {
                        break;
                    }
                    instructions += 1;
                    bytes += approx_insn_bytes(insn, &zp);
                    branched |= is_cond_branch(&insn.mnem) || insn.mnem == "JMP";
                }
                Item::Label(exit) => {
                    if branched && instructions >= 4 {
                        let mut local: HashMap<&str, usize> = labels
                            .iter()
                            .enumerate()
                            .map(|(i, n)| (n.as_str(), i))
                            .collect();
                        local.insert(exit, labels.len());
                        let closed = labels.iter().all(|label| {
                            refs[label]
                                .iter()
                                .all(|r| r.at >= start && r.at < end && r.branch)
                        });
                        if closed {
                            let mut signature = String::new();
                            let mut valid = true;
                            for item in &items[start..end] {
                                match item {
                                    Item::Label(name) => {
                                        signature
                                            .push_str(&format!("@{}:\n", local[name.as_str()]));
                                    }
                                    Item::Insn(insn) => {
                                        if is_cond_branch(&insn.mnem) || insn.mnem == "JMP" {
                                            match insn
                                                .operand
                                                .as_deref()
                                                .and_then(|op| local.get(op))
                                            {
                                                Some(id) => signature
                                                    .push_str(&format!("{} @{id}\n", insn.mnem)),
                                                None => {
                                                    valid = false;
                                                    break;
                                                }
                                            }
                                        } else {
                                            signature.push_str(&format!(
                                                "{} {}\n",
                                                insn.mnem,
                                                insn.operand.as_deref().unwrap_or("")
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if valid {
                                regions.push(Region {
                                    start,
                                    end,
                                    labels: labels.clone(),
                                    exit: exit.clone(),
                                    bytes,
                                    signature,
                                });
                            }
                        }
                    }
                    // Once a label has an outside user, no larger region containing it can become closed.
                    if refs[exit].iter().any(|r| r.at < start || !r.branch) {
                        break;
                    }
                    labels.push(exit.clone());
                }
            }
        }
    }
    regions
}

fn savings(bytes: usize, sites: usize) -> usize {
    (bytes * sites).saturating_sub(bytes + 1 + 3 * sites)
}

pub(super) fn factor(items: &mut Vec<Item>) -> bool {
    let mut groups: BTreeMap<String, Vec<Region>> = BTreeMap::new();
    for region in collect(items) {
        groups
            .entry(region.signature.clone())
            .or_default()
            .push(region);
    }
    let mut ranked = Vec::new();
    for (_, group) in groups {
        let mut disjoint: Vec<Region> = Vec::new();
        for region in group {
            if disjoint.last().is_none_or(|prev| region.start >= prev.end) {
                disjoint.push(region);
            }
        }
        if disjoint.len() >= 2 {
            let gain = savings(disjoint[0].bytes, disjoint.len());
            if gain > 0 {
                ranked.push((gain, disjoint));
            }
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1[0].start.cmp(&b.1[0].start)));
    let mut consumed = HashSet::new();
    let mut edits = Vec::new();
    let mut helpers = Vec::new();
    // Each ID owns its entry, local labels, and exit.
    let mut next_id = items
        .iter()
        .filter_map(|item| {
            if let Item::Label(name) = item {
                name.strip_prefix("__CFG_")?
                    .split('_')
                    .next()?
                    .parse::<u32>()
                    .ok()
            } else {
                None
            }
        })
        .max()
        .map_or(0, |id| id + 1);
    for (_, group) in ranked {
        let sites: Vec<_> = group
            .into_iter()
            .filter(|r| (r.start..r.end).all(|i| !consumed.contains(&i)))
            .collect();
        if sites.len() < 2 || savings(sites[0].bytes, sites.len()) == 0 {
            continue;
        }
        let template = &sites[0];
        let helper = format!("__CFG_{next_id}");
        next_id += 1;
        let locals: HashMap<_, _> = template
            .labels
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), format!("{helper}_{i}")))
            .chain(std::iter::once((
                template.exit.clone(),
                format!("{helper}_RET"),
            )))
            .collect();
        helpers.push(Item::Label(helper.clone()));
        for item in &items[template.start..template.end] {
            match item {
                Item::Label(name) => helpers.push(Item::Label(locals[name].clone())),
                Item::Insn(insn) => {
                    let mut insn = insn.clone();
                    if let Some(op) = &insn.operand
                        && let Some(name) = locals.get(op)
                    {
                        insn.operand = Some(name.clone());
                    }
                    helpers.push(Item::Insn(insn));
                }
                _ => {}
            }
        }
        helpers.push(Item::Label(locals[&template.exit].clone()));
        helpers.push(Item::Insn(Insn {
            mnem: "RTS".into(),
            operand: None,
            comment: None,
        }));
        for site in sites {
            consumed.extend(site.start..site.end);
            edits.push((site.start, site.end, helper.clone()));
        }
    }
    if edits.is_empty() {
        return false;
    }
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, helper) in edits {
        items.splice(
            start..end,
            [Item::Insn(Insn {
                mnem: "JSR".into(),
                operand: Some(helper),
                comment: None,
            })],
        );
    }
    append_helpers_before_heap(items, helpers);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diamonds(prefix: &str, extra: &str) -> String {
        let mut asm = String::from("*=$2000\n");
        for i in 0..4 {
            asm.push_str(&format!(" LDA $0408\n BNE {prefix}_FALSE_{i}\n{extra} LDA #$FF\n JMP {prefix}_END_{i}\n{prefix}_FALSE_{i}:\n LDA #$00\n{prefix}_END_{i}:\n STA ${:04X}\n", 0x0400+i));
        }
        asm.push_str(" JMP $FFFE\n");
        asm
    }

    fn execute(asm: &str, value: u8, status: u8) -> (Vec<u8>, u8, u8, u8, u8) {
        let bytes = asm6502::Assembler6502::new().assemble_bytes(asm).unwrap();
        let mut machine = emu64::System::new();
        machine.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
        machine.mem.ram[0x0400..0x0408].fill(0);
        machine.mem.ram[0x0408] = value;
        machine.start_at(0x2000);
        machine.cpu.regs.sp = 0xE0;
        machine.cpu.regs.p = status;
        machine.cpu.regs.x = 3;
        machine.cpu.regs.y = 0xA5;
        for _ in 0..10_000 {
            if machine.cpu.regs.pc == 0xFFFE {
                break;
            }
            assert!(machine.step());
        }
        assert_eq!(machine.cpu.regs.pc, 0xFFFE);
        assert_eq!(machine.cpu.regs.sp, 0xE0);
        (
            machine.mem.ram[0x0400..0x0408].to_vec(),
            machine.cpu.regs.a,
            machine.cpu.regs.x,
            machine.cpu.regs.y,
            machine.cpu.regs.p,
        )
    }

    #[test]
    fn identical_diamonds_preserve_both_branches_registers_flags_and_stack() {
        let input = diamonds("D", "");
        let mut items = parse(&input);
        assert!(factor(&mut items));
        let output = render(&items);
        assert!(
            asm6502::Assembler6502::new()
                .assemble_bytes(&output)
                .unwrap()
                .len()
                < asm6502::Assembler6502::new()
                    .assemble_bytes(&input)
                    .unwrap()
                    .len()
        );
        for value in 0..=255 {
            for status in [0x20, 0x21, 0x60, 0xE1] {
                assert_eq!(
                    execute(&input, value, status),
                    execute(&output, value, status)
                );
            }
        }
    }

    #[test]
    fn a_branch_at_entry_consumes_the_original_flags_and_loops_stay_internal() {
        let input = diamonds("D", "").replace(" LDA $0408\n", "");
        for branch in ["BNE", "BCS", "BVS"] {
            let input = input.replace("BNE", branch);
            let mut items = parse(&input);
            assert!(factor(&mut items));
            for status in [0x20, 0x22, 0x61, 0xE3] {
                assert_eq!(
                    execute(&input, 0, status),
                    execute(&render(&items), 0, status)
                );
            }
        }
        let mut input = String::from("*=$2000\n");
        for i in 0..3 {
            input.push_str(&format!(
                " LDX #$03\nLOOP_{i}:\n INC $0400\n DEX\n BNE LOOP_{i}\nEND_{i}:\n NOP\n"
            ));
        }
        input.push_str(" JMP $FFFE\n");
        let mut items = parse(&input);
        assert!(factor(&mut items));
        let result = execute(&render(&items), 0, 0x20);
        assert_eq!(result.0[0], 9);
        assert_eq!(result, execute(&input, 0, 0x20));
    }

    #[test]
    fn external_subroutine_entries_remain_callable() {
        let mut input =
            String::from("*=$2000\n JSR ENTRY_0\n JSR ENTRY_1\n JSR ENTRY_2\n JMP $FFFE\n");
        for i in 0..3 {
            input.push_str(&format!("ENTRY_{i}:\n LDA $0408\n BNE FALSE_{i}\n LDA #$FF\n JMP END_{i}\nFALSE_{i}:\n LDA #$00\nEND_{i}:\n STA ${:04X}\n RTS\n", 0x0400+i));
        }
        let mut items = parse(&input);
        assert!(factor(&mut items));
        for value in [0, 1, 255] {
            assert_eq!(
                execute(&input, value, 0x61),
                execute(&render(&items), value, 0x61)
            );
        }

        input.push_str(" .word ENTRY_0,ENTRY_1,ENTRY_2\n");
        let mut items = parse(&input);
        assert!(!factor(&mut items));
    }

    #[test]
    fn outside_entries_address_capture_and_opaque_operations_are_refused() {
        let input = diamonds("D", "");
        for capture in [
            " JMP D_FALSE_0\n JMP D_FALSE_1\n JMP D_FALSE_2\n JMP D_FALSE_3\n",
            " .word D_FALSE_0,D_FALSE_1,D_FALSE_2,D_FALSE_3\n",
            " STA D_FALSE_0+1\n STA D_FALSE_1+1\n STA D_FALSE_2+1\n STA D_FALSE_3+1\n",
        ] {
            let asm = format!("{input}{capture}");
            let mut items = parse(&asm);
            assert!(
                !factor(&mut items),
                "external label user admitted: {capture}"
            );
        }
        for extra in [
            " JSR $FFD2\n",
            " PHA\n PLA\n",
            " TSX\n",
            " LDA $0100,X\n",
            " LDA ($FD),Y\n",
            " STA $01\n",
            " LDA $00FF,X\n",
            " LDA VI_STACK\n",
            " STA VI_PORT\n",
            " STA $FF80,X\n",
        ] {
            let asm = format!("VI_STACK = $0100\nVI_PORT = $01\n{}", diamonds("D", extra));
            let mut items = parse(&asm);
            assert!(!factor(&mut items), "opaque instruction admitted: {extra}");
        }
    }

    #[test]
    fn sys_marked_regions_are_not_outlined_even_when_starting_inside() {
        let input = diamonds("D", "").replace("*=$2000\n", "*=$2000\n; YAB_SYS_WRAP_IN\n")
            + "; YAB_SYS_WRAP_OUT\n";
        let mut items = parse(&input);
        assert!(!factor(&mut items));
    }

    #[test]
    fn integer_and_string_homes_require_data_or_literal_equates() {
        for prefix in ["VI_", "VS_"] {
            let input = diamonds("D", "").replace("$0408", &format!("{prefix}INPUT"));
            let admitted = |source: &str| {
                let items = parse(source);
                let mask = compact_instruction_sites(&items);
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, item)| match item {
                        Item::Insn(insn)
                            if insn.operand.as_deref() == Some(&format!("{prefix}INPUT")) =>
                        {
                            Some(mask[idx])
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            for definition in [
                format!("{prefix}INPUT = $0408\n"),
                format!("{prefix}INPUT:\n .byte 0,0\n"),
            ] {
                assert_eq!(
                    admitted(&(input.clone() + &definition)),
                    vec![true; 4],
                    "home not admitted: {definition}"
                );
            }
            for definition in [
                "".to_string(),
                format!("{prefix}INPUT:\n LDA #$00\n RTS\n"),
                format!("{prefix}INPUT = $0100\n"),
                format!("{prefix}INPUT = MYSTERY\n"),
            ] {
                assert_eq!(
                    admitted(&(input.clone() + &definition)),
                    vec![false; 4],
                    "opaque home admitted: {definition}"
                );
            }
            assert_eq!(
                admitted(
                    &(input.replace("*=$2000", "*=$0080")
                        + &format!("{prefix}INPUT:\n .byte 0,0\n"))
                ),
                vec![false; 4],
                "page-one data layout admitted"
            );
        }
    }

    #[test]
    fn later_round_names_are_unique_and_only_the_final_size_gate_runs() {
        let input = diamonds("D", "");
        let mut items = parse(&format!("{input}__CFG_0:\n RTS\n__CFG_2_0:\n RTS\n"));
        assert!(factor(&mut items));
        let mut seen = HashSet::new();
        for item in &items {
            if let Item::Label(name) = item {
                assert!(seen.insert(name));
            }
        }
        assert!(render(&items).contains("__CFG_3:"));
        for profile in [Profile::Default, Profile::Speed] {
            assert_eq!(
                run_with_options(&input, profile, Options::default()),
                run_with_options(
                    &input,
                    profile,
                    Options {
                        compact_size_dedup: true,
                        ..Options::default()
                    }
                )
            );
        }
        let ordinary = run_with_options(&input, Profile::Size, Options::default());
        assert!(!ordinary.contains("__CFG_"));
        let compact = run_with_options(
            &input,
            Profile::Size,
            Options {
                compact_size_dedup: true,
                aggressive_size_dedup: true,
                ..Options::default()
            },
        );
        for value in [0, 1, 255] {
            assert_eq!(execute(&input, value, 0x61), execute(&compact, value, 0x61));
        }
    }
}
