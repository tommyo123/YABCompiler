//! Compress constants used only as direct PRINT arguments when layout cannot fit.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

const POOL: &str = "; --- string pool (length-prefixed) ---";
/// Marker at the start of packed records.
pub(crate) const DECODER_LABEL: &str = "__CP_BEGIN:";
const VAL_SENTINEL: &str = "; VAL literal sentinel";
// Includes dispatch, decoder, scratch save/restore and long-branch expansion.
const CODE_COST_BOUND: usize = 224;
const COMPACT_CODE_COST_BOUND: usize = 256;

#[derive(Clone)]
struct Record {
    name: String,
    bytes: Vec<u8>,
    val_sentinel: bool,
    tokens: Vec<(u8, Option<(u8, u8)>)>,
    packed: Vec<u8>,
}

#[derive(Clone)]
struct Node {
    count: usize,
    minimum: u8,
    leaf: Option<u8>,
    children: Option<(Box<Node>, Box<Node>)>,
}

fn code(line: &str) -> &str {
    line.split(';').next().unwrap_or("").trim()
}

fn string_name(s: &str) -> bool {
    s.len() > 1 && s.starts_with('S') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

fn parse_records(lines: &[&str]) -> Option<Vec<Record>> {
    let mut records: Vec<Record> = Vec::new();
    let mut names = BTreeSet::new();
    for line in lines {
        if line.trim() == VAL_SENTINEL {
            let record = records.last_mut()?;
            if record.val_sentinel || !record.bytes.is_empty() {
                return None;
            }
            record.val_sentinel = true;
            continue;
        }
        let line = code(line);
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_suffix(':') {
            if !string_name(name) || !names.insert(name.to_owned()) {
                return None;
            }
            records.push(Record {
                name: name.to_owned(),
                bytes: Vec::new(),
                val_sentinel: false,
                tokens: Vec::new(),
                packed: Vec::new(),
            });
        } else {
            let bytes = &mut records.last_mut()?.bytes;
            for value in line.strip_prefix(".byte ")?.split(',') {
                let hex = value.trim().strip_prefix('$')?;
                if hex.len() != 2 {
                    return None;
                }
                bytes.push(u8::from_str_radix(hex, 16).ok()?);
            }
        }
    }
    if records
        .iter()
        .any(|r| r.bytes.is_empty()
            || r.bytes.len() != usize::from(r.bytes[0]) + 1 + usize::from(r.val_sentinel)
            || (r.val_sentinel && r.bytes.last() != Some(&0)))
    {
        return None;
    }
    Some(records)
}

fn tokens(bytes: &[u8]) -> Vec<(u8, Option<(u8, u8)>)> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut end = i + 1;
        while end < bytes.len() && bytes[end] == bytes[i] {
            end += 1;
        }
        if end - i >= 6 || bytes[i] == 0 {
            result.push((0, Some(((end - i) as u8, bytes[i]))));
        } else {
            result.extend((i..end).map(|_| (bytes[i], None)));
        }
        i = end;
    }
    result
}

fn tree(
    node: &Node,
    bits: &mut Vec<bool>,
    alphabet: &[u8],
    left: &mut Vec<u8>,
    right: &mut Vec<u8>,
    codes: &mut BTreeMap<u8, Vec<bool>>,
) -> u8 {
    if let Some(symbol) = node.leaf {
        codes.insert(symbol, bits.clone());
        return 0x80 + alphabet.binary_search(&symbol).unwrap() as u8;
    }
    let id = left.len();
    left.push(0);
    right.push(0);
    let (a, b) = node.children.as_ref().unwrap();
    bits.push(false);
    left[id] = tree(a, bits, alphabet, left, right, codes);
    *bits.last_mut().unwrap() = true;
    right[id] = tree(b, bits, alphabet, left, right, codes);
    bits.pop();
    id as u8
}

fn byte_line(out: &mut String, bytes: &[u8]) {
    out.push_str("    .byte ");
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 {
            out.push(',');
        }
        write!(out, "${b:02X}").unwrap();
    }
    out.push('\n');
}

struct Pool<'a> {
    lines: Vec<&'a str>,
    start: usize,
    end: usize,
    records: Vec<Record>,
    eligible: BTreeSet<String>,
}

fn parse_pool(source: &str) -> Option<Pool<'_>> {
    if source.contains("__CP_") {
        return None;
    }
    let lines: Vec<_> = source.lines().collect();
    let start = lines.iter().position(|l| *l == POOL)?;
    let end = (start + 1..lines.len()).find(|&i| lines[i].starts_with("; ---"))?;
    let records = parse_records(&lines[start + 1..end])?;
    // A PH326 entry in front of the trampoline reaches it like a direct call does.
    let entries = crate::peephole::str_print_inline_entry_names(source);
    let printers = register_print_entries(&lines);
    let mut refs: BTreeMap<_, (usize, bool)> = records
        .iter()
        .map(|r| (r.name.as_str(), (0, true)))
        .collect();
    for (i, line) in lines.iter().enumerate() {
        let line = code(line);
        for name in line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if let Some((count, safe)) = refs.get_mut(name) {
                if line == format!("{name}:") {
                    continue;
                }
                *count += 1;
                let inline = line == format!(".word {name}")
                    && i > 0
                    && code(lines[i - 1]).strip_prefix("JSR ").is_some_and(|target| {
                        target == crate::peephole::STR_PRINT_INLINE_ENTRY || entries.contains(target)
                    });
                *safe &= inline || printed_from_registers(&lines, i, name, &printers);
            }
        }
    }
    let eligible: BTreeSet<String> = records
        .iter()
        .filter(|r| {
            let (count, safe) = refs[r.name.as_str()];
            count > 0 && safe && r.bytes.len() > 2 && !r.val_sentinel
        })
        .map(|r| r.name.clone())
        .collect();
    drop(refs);
    Some(Pool {
        lines,
        start,
        end,
        records,
        eligible,
    })
}

/// Find entries that print the string pointer passed in A/Y.
fn register_print_entries(lines: &[&str]) -> BTreeSet<String> {
    let mut printers = BTreeSet::from(["__STR_PRINT".to_owned()]);
    let mut pending: Vec<&str> = Vec::new();
    for line in lines {
        let line = code(line);
        if line.is_empty() {
            continue;
        }
        if let Some(label) = line.strip_suffix(':') {
            pending.push(label);
            continue;
        }
        let prints = line == "JSR __STR_PRINT"
            || line == "JMP __STR_PRINT"
            || pending.contains(&"__STR_PRINT");
        if prints {
            printers.extend(pending.iter().map(|l| (*l).to_owned()));
        }
        pending.clear();
    }
    printers
}

fn next_code<'a>(lines: &[&'a str], i: usize) -> Option<(usize, &'a str)> {
    (i + 1..lines.len()).map(|k| (k, code(lines[k]))).find(|(_, l)| !l.is_empty())
}

fn prev_code<'a>(lines: &[&'a str], i: usize) -> Option<(usize, &'a str)> {
    (0..i).rev().map(|k| (k, code(lines[k]))).find(|(_, l)| !l.is_empty())
}

/// Whether the pointer load feeds only a string print call.
fn printed_from_registers(lines: &[&str], i: usize, name: &str, printers: &BTreeSet<String>) -> bool {
    let (lo, hi) = (format!("LDA #<{name}"), format!("LDY #>{name}"));
    let line = code(lines[i]);
    let first = if line == lo || line == hi {
        let other = if line == lo { &hi } else { &lo };
        match (prev_code(lines, i), next_code(lines, i)) {
            (Some((k, prev)), _) if prev == other => k,
            (_, Some((_, next))) if next == other => i,
            _ => return false,
        }
    } else {
        return false;
    };
    let Some((second, _)) = next_code(lines, first) else {
        return false;
    };
    let mut at = second;
    loop {
        let Some((k, next)) = next_code(lines, at) else {
            return false;
        };
        if next.ends_with(':') {
            at = k;
            continue;
        }
        return next
            .strip_prefix("JSR ")
            .or_else(|| next.strip_prefix("JMP "))
            .is_some_and(|target| printers.contains(target));
    }
}

pub(crate) fn compress(source: &str) -> Option<String> {
    let Pool {
        lines,
        start,
        end,
        mut records,
        eligible,
    } = parse_pool(source)?;
    let mut frequencies = BTreeMap::<u8, usize>::new();
    for r in records.iter_mut().filter(|r| eligible.contains(&r.name)) {
        r.tokens = tokens(&r.bytes[1..]);
        for (symbol, _) in &r.tokens {
            *frequencies.entry(*symbol).or_default() += 1;
        }
    }
    // Leaves occupy the high-bit namespace; internal nodes occupy the low one.
    if !(2..=128).contains(&frequencies.len()) {
        return None;
    }
    let alphabet: Vec<_> = frequencies.keys().copied().collect();
    let mut nodes: Vec<_> = frequencies
        .into_iter()
        .map(|(symbol, count)| Node {
            count,
            minimum: symbol,
            leaf: Some(symbol),
            children: None,
        })
        .collect();
    while nodes.len() > 1 {
        nodes.sort_by_key(|n| (n.count, n.minimum));
        let a = nodes.remove(0);
        let b = nodes.remove(0);
        nodes.push(Node {
            count: a.count + b.count,
            minimum: a.minimum.min(b.minimum),
            leaf: None,
            children: Some((Box::new(a), Box::new(b))),
        });
    }
    let (mut left, mut right, mut codes) = (Vec::new(), Vec::new(), BTreeMap::new());
    tree(
        &nodes[0],
        &mut Vec::new(),
        &alphabet,
        &mut left,
        &mut right,
        &mut codes,
    );
    let mut saving = 0;
    for r in records.iter_mut().filter(|r| eligible.contains(&r.name)) {
        let mut bits = Vec::new();
        for (symbol, extra) in &r.tokens {
            bits.extend_from_slice(&codes[symbol]);
            if let Some((count, byte)) = extra {
                for value in [count, byte] {
                    bits.extend((0..8).rev().map(|shift| value & (1 << shift) != 0));
                }
            }
        }
        r.packed.push(r.bytes[0]);
        r.packed.extend(bits.chunks(8).map(|chunk| {
            (0..8).fold(0, |v, bit| {
                (v << 1) | u8::from(chunk.get(bit).copied().unwrap_or(false))
            })
        }));
        if r.packed.len() >= r.bytes.len() {
            r.packed.clear();
        } else {
            saving += r.bytes.len() - r.packed.len();
        }
    }
    if saving <= left.len() + right.len() + alphabet.len() + 7 + CODE_COST_BOUND {
        return None;
    }
    emit_pool(
        &lines,
        start,
        end,
        &records,
        &[("LEFT", &left), ("RIGHT", &right), ("VALUES", &alphabet)],
        DECODER,
    )
}

struct CompactModel {
    records: Vec<Record>,
    lengths: BTreeMap<u8, usize>,
    counts: Vec<u8>,
    values: Vec<u8>,
    saving: usize,
}

impl CompactModel {
    fn score(&self) -> isize {
        self.saving as isize - self.counts.len() as isize - self.values.len() as isize
    }
}

fn adaptive_tokens(bytes: &[u8], lengths: &BTreeMap<u8, usize>) -> Vec<(u8, Option<(u8, u8)>)> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut end = i + 1;
        while end < bytes.len() && bytes[end] == bytes[i] {
            end += 1;
        }
        let count = end - i;
        let literal = lengths.get(&bytes[i]).map(|length| length * count);
        let escape = lengths.get(&0).map(|length| length + 16);
        let use_escape = bytes[i] == 0
            || match (literal, escape) {
                (Some(literal), Some(escape)) => escape <= literal,
                (None, Some(_)) => true,
                _ => false,
            };
        if use_escape {
            result.push((0, Some((count as u8, bytes[i]))));
        } else {
            result.extend((i..end).map(|_| (bytes[i], None)));
        }
        i = end;
    }
    result
}

fn compact_model(
    records: &[Record],
    active: &BTreeSet<String>,
    previous: Option<&BTreeMap<u8, usize>>,
) -> Option<CompactModel> {
    let mut records = records.to_vec();
    let mut frequencies = BTreeMap::<u8, usize>::new();
    for r in records.iter_mut().filter(|r| active.contains(&r.name)) {
        r.tokens = previous.map_or_else(
            || tokens(&r.bytes[1..]),
            |lengths| adaptive_tokens(&r.bytes[1..], lengths),
        );
        for (symbol, _) in &r.tokens {
            *frequencies.entry(*symbol).or_default() += 1;
        }
    }
    if !(1..=128).contains(&frequencies.len()) {
        return None;
    }
    let alphabet: Vec<_> = frequencies.keys().copied().collect();
    let mut nodes: Vec<_> = frequencies
        .into_iter()
        .map(|(symbol, count)| Node {
            count,
            minimum: symbol,
            leaf: Some(symbol),
            children: None,
        })
        .collect();
    while nodes.len() > 1 {
        nodes.sort_by_key(|n| (n.count, n.minimum));
        let a = nodes.remove(0);
        let b = nodes.remove(0);
        nodes.push(Node {
            count: a.count + b.count,
            minimum: a.minimum.min(b.minimum),
            leaf: None,
            children: Some((Box::new(a), Box::new(b))),
        });
    }
    let mut codes = BTreeMap::new();
    tree(
        &nodes[0],
        &mut Vec::new(),
        &alphabet,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut codes,
    );
    // A one-symbol model still consumes one zero bit per token.
    let lengths: BTreeMap<_, _> = codes
        .iter()
        .map(|(&symbol, bits)| (symbol, bits.len().max(1)))
        .collect();
    let mut values = alphabet;
    values.sort_by_key(|symbol| (lengths[symbol], *symbol));
    let maximum = *lengths.values().max()?;
    if maximum > 127 {
        return None;
    }
    let mut counts = vec![0u8; maximum];
    let mut value = 0u128;
    let mut previous_length = 0;
    for &symbol in &values {
        let length = lengths[&symbol];
        value = value.checked_shl((length - previous_length) as u32)?;
        codes.insert(
            symbol,
            (0..length)
                .rev()
                .map(|bit| value & (1u128 << bit) != 0)
                .collect(),
        );
        counts[length - 1] += 1;
        value = value.checked_add(1)?;
        previous_length = length;
    }
    let mut saving = 0;
    for r in records.iter_mut().filter(|r| active.contains(&r.name)) {
        let mut bits = Vec::new();
        for (symbol, extra) in &r.tokens {
            bits.extend_from_slice(&codes[symbol]);
            if let Some((count, byte)) = extra {
                for value in [count, byte] {
                    bits.extend((0..8).rev().map(|shift| value & (1 << shift) != 0));
                }
            }
        }
        r.packed.push(r.bytes[0]);
        r.packed.extend(bits.chunks(8).map(|chunk| {
            (0..8).fold(0, |v, bit| {
                (v << 1) | u8::from(chunk.get(bit).copied().unwrap_or(false))
            })
        }));
        if r.packed.len() >= r.bytes.len() {
            r.packed.clear();
        } else {
            saving += r.bytes.len() - r.packed.len();
        }
    }
    Some(CompactModel {
        records,
        lengths,
        counts,
        values,
        saving,
    })
}

/// Byte symbol or pair index plus 256.
type Sym = u16;

/// Limit recursive pair expansion to protect the stack.
const MAX_PAIR_DEPTH: usize = 10;
/// The tree decoder and dispatch, with the eight state bytes.
const TREE_DECODER_COST: usize = 192 + 8;
/// The canonical decoder and dispatch, with the eight state bytes.
const CANONICAL_DECODER_COST: usize = 216 + 8;
/// What pair expansion adds to the tree decoder.
const PAIR_DECODER_COST: usize = 47;

#[derive(Clone)]
struct SymNode {
    count: usize,
    minimum: Sym,
    leaf: Option<Sym>,
    children: Option<(Box<SymNode>, Box<SymNode>)>,
}

/// Huffman tree over `freqs` (at least two symbols), ties broken as `compress` breaks them.
fn sym_huffman(freqs: &BTreeMap<Sym, usize>) -> SymNode {
    let mut nodes: Vec<_> = freqs
        .iter()
        .map(|(&symbol, &count)| SymNode {
            count,
            minimum: symbol,
            leaf: Some(symbol),
            children: None,
        })
        .collect();
    while nodes.len() > 1 {
        nodes.sort_by_key(|n| (n.count, n.minimum));
        let a = nodes.remove(0);
        let b = nodes.remove(0);
        nodes.push(SymNode {
            count: a.count + b.count,
            minimum: a.minimum.min(b.minimum),
            leaf: None,
            children: Some((Box::new(a), Box::new(b))),
        });
    }
    nodes.remove(0)
}

fn sym_lengths(node: &SymNode, depth: usize, out: &mut BTreeMap<Sym, usize>) {
    match (&node.leaf, &node.children) {
        (Some(symbol), _) => {
            out.insert(*symbol, depth);
        }
        (None, Some((a, b))) => {
            sym_lengths(a, depth + 1, out);
            sym_lengths(b, depth + 1, out);
        }
        _ => {}
    }
}

/// Build decoder tables and collect symbol codes.
fn sym_tree(
    node: &SymNode,
    bits: &mut Vec<bool>,
    index: &BTreeMap<Sym, u8>,
    left: &mut Vec<u8>,
    right: &mut Vec<u8>,
    codes: &mut BTreeMap<Sym, Vec<bool>>,
) -> u8 {
    if let Some(symbol) = node.leaf {
        codes.insert(symbol, bits.clone());
        return 0x80 + index[&symbol];
    }
    let id = left.len();
    left.push(0);
    right.push(0);
    let (a, b) = node.children.as_ref().unwrap();
    bits.push(false);
    left[id] = sym_tree(a, bits, index, left, right, codes);
    *bits.last_mut().unwrap() = true;
    right[id] = sym_tree(b, bits, index, left, right, codes);
    bits.pop();
    id as u8
}

type SymTokens = Vec<(Sym, Option<(u8, u8)>)>;

/// Bytes followed by reachable pairs in expansion order.
fn pair_alphabet(seqs: &[SymTokens], pairs: &[(Sym, Sym)]) -> Vec<Sym> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<Sym> = seqs.iter().flatten().map(|(s, _)| *s).collect();
    while let Some(symbol) = stack.pop() {
        if seen.insert(symbol) && symbol >= 256 {
            let (a, b) = pairs[usize::from(symbol - 256)];
            stack.extend([a, b]);
        }
    }
    seen.into_iter().collect()
}

/// Packed record, table and decoder sizes.
fn pair_cost(seqs: &[SymTokens], pairs: &[(Sym, Sym)], raw: &[usize]) -> Option<(usize, Vec<Option<usize>>)> {
    let mut freqs = BTreeMap::<Sym, usize>::new();
    for (symbol, _) in seqs.iter().flatten() {
        *freqs.entry(*symbol).or_default() += 1;
    }
    let alphabet = pair_alphabet(seqs, pairs);
    if freqs.len() < 2 || alphabet.len() > 128 {
        return None;
    }
    let mut lengths = BTreeMap::new();
    sym_lengths(&sym_huffman(&freqs), 0, &mut lengths);
    let mut total = 0;
    let mut sizes = Vec::with_capacity(seqs.len());
    for (seq, &raw) in seqs.iter().zip(raw) {
        let bits: usize = seq.iter().map(|(s, e)| lengths[s] + if e.is_some() { 16 } else { 0 }).sum();
        let packed = 1 + bits.div_ceil(8);
        sizes.push((packed < raw).then_some(packed));
        total += packed.min(raw);
    }
    let reached_pairs = alphabet.iter().filter(|&&s| s >= 256).count();
    let chars = alphabet.len() - reached_pairs;
    total += chars + 2 * (freqs.len() - 1) + 2 * reached_pairs + TREE_DECODER_COST;
    if reached_pairs > 0 {
        total += PAIR_DECODER_COST;
    }
    Some((total, sizes))
}

struct PairModel {
    records: Vec<Record>,
    tables: Vec<(&'static str, Vec<u8>)>,
    chars: usize,
    pairs: usize,
    /// Bytes saved against leaving the eligible records raw, decoder and tables paid.
    net: isize,
}

/// Replace common byte pairs while the total encoded size shrinks.
fn pair_model(records: &[Record], eligible: &BTreeSet<String>) -> Option<PairModel> {
    let which: Vec<usize> = (0..records.len()).filter(|&i| eligible.contains(&records[i].name)).collect();
    let raw: Vec<usize> = which.iter().map(|&i| records[i].bytes.len()).collect();
    let mut seqs: Vec<SymTokens> = which
        .iter()
        .map(|&i| tokens(&records[i].bytes[1..]).into_iter().map(|(s, e)| (Sym::from(s), e)).collect())
        .collect();
    let mut pairs: Vec<(Sym, Sym)> = Vec::new();
    let mut depth: Vec<usize> = Vec::new();
    let depth_of = |depth: &[usize], s: Sym| if s >= 256 { depth[usize::from(s - 256)] } else { 0 };
    let (mut best_cost, _) = pair_cost(&seqs, &pairs, &raw)?;
    let mut best = (seqs.clone(), pairs.clone());
    let mut banned = BTreeSet::new();
    while pairs.len() < 127 {
        let mut counts = BTreeMap::<(Sym, Sym), usize>::new();
        for seq in &seqs {
            let mut i = 0;
            while i + 1 < seq.len() {
                let ((a, ea), (b, eb)) = (seq[i], seq[i + 1]);
                if ea.is_none() && eb.is_none() {
                    *counts.entry((a, b)).or_default() += 1;
                    if a == b {
                        i += 1;
                    }
                }
                i += 1;
            }
        }
        let Some((&(a, b), &count)) = counts
            .iter()
            .filter(|(pair, _)| !banned.contains(*pair))
            .filter(|((a, b), _)| 1 + depth_of(&depth, *a).max(depth_of(&depth, *b)) <= MAX_PAIR_DEPTH)
            .max_by_key(|((a, b), count)| (**count, std::cmp::Reverse((*a, *b))))
        else {
            break;
        };
        if count < 3 {
            break;
        }
        let symbol = 256 + pairs.len() as Sym;
        let next: Vec<SymTokens> = seqs
            .iter()
            .map(|seq| {
                let mut out = Vec::with_capacity(seq.len());
                let mut i = 0;
                while i < seq.len() {
                    if i + 1 < seq.len() && seq[i] == (a, None) && seq[i + 1] == (b, None) {
                        out.push((symbol, None));
                        i += 2;
                    } else {
                        out.push(seq[i]);
                        i += 1;
                    }
                }
                out
            })
            .collect();
        let mut grown = pairs.clone();
        grown.push((a, b));
        let Some((cost, _)) = pair_cost(&next, &grown, &raw) else {
            banned.insert((a, b));
            continue;
        };
        depth.push(1 + depth_of(&depth, a).max(depth_of(&depth, b)));
        seqs = next;
        pairs = grown;
        if cost < best_cost {
            best_cost = cost;
            best = (seqs.clone(), pairs.clone());
        }
    }
    let (seqs, pairs) = best;
    let alphabet = pair_alphabet(&seqs, &pairs);
    let index: BTreeMap<Sym, u8> = alphabet.iter().enumerate().map(|(i, &s)| (s, i as u8)).collect();
    let chars = alphabet.iter().filter(|&&s| s < 256).count();
    let mut freqs = BTreeMap::<Sym, usize>::new();
    for (symbol, _) in seqs.iter().flatten() {
        *freqs.entry(*symbol).or_default() += 1;
    }
    let (mut left, mut right, mut codes) = (Vec::new(), Vec::new(), BTreeMap::new());
    sym_tree(&sym_huffman(&freqs), &mut Vec::new(), &index, &mut left, &mut right, &mut codes);
    if left.len() > 127 {
        return None;
    }
    let mut records = records.to_vec();
    let mut saving = 0isize;
    for (seq, &i) in seqs.iter().zip(&which) {
        let r = &mut records[i];
        let mut bits = Vec::new();
        for (symbol, extra) in seq {
            bits.extend_from_slice(&codes[symbol]);
            if let Some((count, byte)) = extra {
                for value in [count, byte] {
                    bits.extend((0..8).rev().map(|shift| value & (1 << shift) != 0));
                }
            }
        }
        r.packed = vec![r.bytes[0]];
        r.packed.extend(bits.chunks(8).map(|chunk| {
            (0..8).fold(0, |v, bit| (v << 1) | u8::from(chunk.get(bit).copied().unwrap_or(false)))
        }));
        if r.packed.len() >= r.bytes.len() {
            r.packed.clear();
        } else {
            saving += (r.bytes.len() - r.packed.len()) as isize;
        }
    }
    let values: Vec<u8> = alphabet.iter().filter(|&&s| s < 256).map(|&s| s as u8).collect();
    let pair_tables: (Vec<u8>, Vec<u8>) = alphabet
        .iter()
        .filter(|&&s| s >= 256)
        .map(|&s| {
            let (a, b) = pairs[usize::from(s - 256)];
            (index[&a], index[&b])
        })
        .unzip();
    let pair_count = pair_tables.0.len();
    let mut cost = TREE_DECODER_COST + values.len() + left.len() + right.len();
    if pair_count > 0 {
        cost += PAIR_DECODER_COST + 2 * pair_count;
    }
    let mut tables = vec![("LEFT", left), ("RIGHT", right), ("VALUES", values)];
    if pair_count > 0 {
        tables.push(("PAIRL", pair_tables.0));
        tables.push(("PAIRR", pair_tables.1));
    }
    Some(PairModel {
        records,
        tables,
        chars,
        pairs: pair_count,
        net: saving - cost as isize,
    })
}

/// Expand pair symbols through recursive printing.
fn pair_decoder(chars: usize) -> String {
    let c = format!("${chars:02X}");
    DECODER
        .replacen(
            "__CP_LEAF:\n    AND #$7F\n    TAX\n",
            &format!("__CP_LEAF:\n    AND #$7F\n    CMP #{c}\n    BCS __CP_LEAF_PAIR\n    TAX\n"),
            1,
        )
        .replacen(
            "    BNE __CP_NEXT\n__CP_DONE:\n",
            "    BNE __CP_NEXT\n    BEQ __CP_DONE\n__CP_LEAF_PAIR:\n    JSR __CP_EMIT\n    LDX __CP_STATE\n    BEQ __CP_DONE\n    JMP __CP_NEXT\n__CP_DONE:\n",
            1,
        )
        + &format!(
            "__CP_EMIT:\n    CMP #{c}\n    BCS __CP_EMIT_PAIR\n    TAX\n    LDA __CP_VALUES,X\n    JSR $FFD2\n    DEC __CP_STATE\n    RTS\n\
             __CP_EMIT_PAIR:\n    TAX\n    LDA __CP_PAIRR-{c},X\n    PHA\n    LDA __CP_PAIRL-{c},X\n    JSR __CP_EMIT\n    PLA\n    JMP __CP_EMIT\n"
        )
}

/// Choose the smaller packing after including its decoder cost.
pub(crate) fn compress_size(source: &str, other: bool) -> Option<String> {
    let Pool {
        lines,
        start,
        end,
        records,
        eligible,
    } = parse_pool(source)?;
    let pair = pair_model(&records, &eligible);
    let canonical = compact_model(&records, &eligible, None).map(|m| {
        let net = m.score() - CANONICAL_DECODER_COST as isize;
        (m, net)
    });
    let pair_net = pair.as_ref().map_or(isize::MIN, |m| m.net);
    let canonical_net = canonical.as_ref().map_or(isize::MIN, |(_, net)| *net);
    let take_pair = (pair_net >= canonical_net) != other;
    if (if take_pair { pair_net } else { canonical_net }) <= 0 {
        return None;
    }
    if take_pair {
        let model = pair?;
        let decoder = if model.pairs > 0 { pair_decoder(model.chars) } else { DECODER.to_owned() };
        let tables: Vec<(&str, &[u8])> = model.tables.iter().map(|(n, t)| (*n, t.as_slice())).collect();
        emit_pool(&lines, start, end, &model.records, &tables, &decoder)
    } else {
        let (model, _) = canonical?;
        let decoder = DECODER.replacen(TREE_NODE, CANONICAL_NODE, 1);
        emit_pool(
            &lines,
            start,
            end,
            &model.records,
            &[("COUNTS", &model.counts), ("VALUES", &model.values)],
            &decoder,
        )
    }
}

/// Extra model search for the final Compact retry.
pub(crate) fn compress_compact(source: &str) -> Option<String> {
    let Pool {
        lines,
        start,
        end,
        records,
        eligible,
    } = parse_pool(source)?;
    let initial = compact_model(&records, &eligible, None)?;
    let old_score = (initial.values.len() > 1)
        .then(|| initial.saving as isize - (3 * initial.values.len() - 2) as isize);
    let mut best = initial;
    // Keep the best byte-rounded result, including the original model.
    for prune in [false, true] {
        let mut active = eligible.clone();
        let mut previous = None;
        for _ in 0..8 {
            let Some(model) = compact_model(&records, &active, previous.as_ref()) else {
                break;
            };
            let stable = previous.as_ref() == Some(&model.lengths);
            previous = Some(model.lengths.clone());
            let mut pruned = false;
            if prune {
                let before = active.len();
                active.retain(|name| {
                    model
                        .records
                        .iter()
                        .any(|r| &r.name == name && !r.packed.is_empty())
                });
                pruned = active.len() != before;
            }
            if model.score() > best.score() {
                best = model;
            }
            if stable && !pruned {
                break;
            }
        }
    }
    if best.score() <= (7 + COMPACT_CODE_COST_BOUND) as isize
        || old_score.is_some_and(|old| {
            best.score() - old <= (COMPACT_CODE_COST_BOUND - CODE_COST_BOUND) as isize
        })
    {
        return None;
    }
    let decoder = DECODER.replacen(TREE_NODE, CANONICAL_NODE, 1);
    emit_pool(
        &lines,
        start,
        end,
        &best.records,
        &[("COUNTS", &best.counts), ("VALUES", &best.values)],
        &decoder,
    )
}

fn print_pointer(lines: &[&str]) -> Option<(String, String)> {
    fn operand<'a>(line: &'a str, mnemonic: &str) -> Option<&'a str> {
        let (op, arg) = line.split_once(char::is_whitespace)?;
        (op == mnemonic).then_some(arg.trim())
    }
    let body: Vec<_> = lines.iter().map(|line| code(line)).filter(|line| !line.is_empty()).collect();
    let mut entries = body.iter().enumerate().filter(|(_, line)| **line == "__STR_PRINT_PTR:");
    let (at, _) = entries.next()?;
    if entries.next().is_some() || operand(body.get(at + 1)?, "LDY")? != "#$00" {
        return None;
    }
    let arg = operand(body.get(at + 2)?, "LDA")?;
    let hex = arg.strip_prefix("($")?.strip_suffix("),Y")?;
    if hex.len() != 2 {
        return None;
    }
    let low = u8::from_str_radix(hex, 16).ok()?;
    let high = low.checked_add(1)?;
    if low < 2 {
        return None;
    }
    let (lo, hi) = (format!("${low:02X}"), format!("${high:02X}"));
    let mut register_entries = body.iter().enumerate().filter(|(_, line)| **line == "__STR_PRINT:");
    if let Some((entry, _)) = register_entries.next() {
        if register_entries.next().is_some()
            || operand(body.get(entry + 1)?, "STA")? != lo
            || operand(body.get(entry + 2)?, "STY")? != hi
            || entry + 3 != at
        {
            return None;
        }
    }
    Some((lo, hi))
}

fn bind_pointer(template: &str, low: &str, high: &str) -> String {
    // Placeholders avoid cascading FD->FE, FE->FF substitutions.
    template.replace("@PRINT_LO@", low).replace("@PRINT_HI@", high)
}

fn emit_pool(
    lines: &[&str],
    start: usize,
    end: usize,
    records: &[Record],
    tables: &[(&str, &[u8])],
    decoder: &str,
) -> Option<String> {
    let (low, high) = print_pointer(lines)?;
    let dispatch = bind_pointer(DISPATCH, &low, &high);
    let decoder = bind_pointer(decoder, &low, &high);
    let mut pool = format!(
        "{POOL}\n; Private packed PRINT records, followed by ordinary strings.\n__CP_BEGIN:\n"
    );
    for r in records.iter().filter(|r| !r.packed.is_empty()) {
        writeln!(pool, "{}:", r.name).unwrap();
        byte_line(&mut pool, &r.packed);
    }
    pool.push_str("__CP_END:\n");
    for r in records.iter().filter(|r| r.packed.is_empty()) {
        writeln!(pool, "{}:", r.name).unwrap();
        if r.val_sentinel {
            writeln!(pool, "{VAL_SENTINEL}").unwrap();
        }
        byte_line(&mut pool, &r.bytes);
    }
    for (name, bytes) in tables {
        writeln!(pool, "__CP_{name}:").unwrap();
        byte_line(&mut pool, bytes);
    }
    pool.push_str("__CP_STATE:\n    .byte $00,$00,$00,$00,$00,$00,$00\n__CP_RET:\n    .byte $00\n");
    let mut output = lines[..start].join("\n") + "\n" + &pool + &lines[end..].join("\n") + "\n";
    if output.matches("__STR_PRINT_PTR:\n").count() != 1 {
        return None;
    }
    output = output.replacen("__STR_PRINT_PTR:\n", &dispatch, 1);
    // BSS suffixes are zeroed/trimmed by later passes; decoder instructions must be resident code preceding that suffix, and preceding HEAP_BOTTOM.
    let marker = if output.contains("__BSS_START:\n") {
        Some("__BSS_START:\n")
    } else if output.contains("__HEAP_BOTTOM:\n") {
        Some("__HEAP_BOTTOM:\n")
    } else {
        None
    };
    if let Some(marker) = marker {
        if output.matches(marker).count() != 1 {
            return None;
        }
        output = output.replacen(marker, &(decoder.to_owned() + marker), 1);
    } else {
        // A PRINT-only module has no heap/BSS markers.
        output.push_str(&decoder);
    }
    Some(output)
}

const DISPATCH: &str = "__STR_PRINT_PTR:
    PHP
    LDA @PRINT_HI@
    CMP #>__CP_BEGIN
    BCC __CP_RAW
    BNE __CP_CHECK_END
    LDA @PRINT_LO@
    CMP #<__CP_BEGIN
    BCC __CP_RAW
__CP_CHECK_END:
    LDA @PRINT_HI@
    CMP #>__CP_END
    BCC __CP_PACKED
    BNE __CP_RAW
    LDA @PRINT_LO@
    CMP #<__CP_END
    BCC __CP_PACKED
__CP_RAW:
    PLP
";

const TREE_NODE: &str = "__CP_NEXT:
    LDX #$00
__CP_NODE:
    JSR __CP_BIT
    BCS __CP_TAKE_RIGHT
    LDA __CP_LEFT,X
    JMP __CP_EDGE
__CP_TAKE_RIGHT:
    LDA __CP_RIGHT,X
__CP_EDGE:
    BMI __CP_LEAF
    TAX
    JMP __CP_NODE
__CP_LEAF:
    AND #$7F
    TAX
    LDA __CP_VALUES,X";

// Canonical Huffman decoding subtracts the leaf count at each depth.
const CANONICAL_NODE: &str = "__CP_NEXT:
    PHP
    CLD
    LDX #$00
    STX __CP_STATE+6
    STX __CP_STATE+5
__CP_NODE:
    JSR __CP_BIT
    ROL __CP_STATE+5
    LDA __CP_STATE+5
    CMP __CP_COUNTS,X
    BCC __CP_LEAF
    SBC __CP_COUNTS,X
    STA __CP_STATE+5
    LDA __CP_STATE+6
    CLC
    ADC __CP_COUNTS,X
    STA __CP_STATE+6
    INX
    BNE __CP_NODE
__CP_LEAF:
    CLC
    ADC __CP_STATE+6
    TAX
    PLP
    LDA __CP_VALUES,X";

const DECODER: &str = "__CP_PACKED:
    PLP
    LDX #$06
__CP_SAVE:
    LDA __CP_STATE,X
    PHA
    DEX
    BPL __CP_SAVE
    LDY #$00
    LDA (@PRINT_LO@),Y
    STA __CP_STATE
    STA __CP_STATE+1
    INY
    LDA #$00
    STA __CP_STATE+2
__CP_NEXT:
    LDX #$00
__CP_NODE:
    JSR __CP_BIT
    BCS __CP_TAKE_RIGHT
    LDA __CP_LEFT,X
    JMP __CP_EDGE
__CP_TAKE_RIGHT:
    LDA __CP_RIGHT,X
__CP_EDGE:
    BMI __CP_LEAF
    TAX
    JMP __CP_NODE
__CP_LEAF:
    AND #$7F
    TAX
    LDA __CP_VALUES,X
    BNE __CP_LITERAL
    JSR __CP_READ_BYTE
    STA __CP_STATE+4
    JSR __CP_READ_BYTE
    STA __CP_STATE+5
__CP_REPEAT:
    LDA __CP_STATE+5
    JSR $FFD2
    DEC __CP_STATE
    DEC __CP_STATE+4
    BNE __CP_REPEAT
    LDX __CP_STATE
    BNE __CP_NEXT
    BEQ __CP_DONE
__CP_LITERAL:
    JSR $FFD2
    DEC __CP_STATE
    BNE __CP_NEXT
__CP_DONE:
    LDY __CP_STATE+1
    INY
    STA __CP_RET
    LDX #$F9
__CP_RESTORE:
    PLA
    STA __CP_STATE-$F9,X
    INX
    BNE __CP_RESTORE
    LDA __CP_RET
    LDX #$00
    RTS
__CP_BIT:
    LDA __CP_STATE+2
    BNE __CP_SHIFT
    LDA (@PRINT_LO@),Y
    INY
    STA __CP_STATE+3
    LDA #$08
    STA __CP_STATE+2
__CP_SHIFT:
    ASL __CP_STATE+3
    DEC __CP_STATE+2
    RTS
__CP_READ_BYTE:
    LDA #$00
    STA __CP_STATE+6
    LDX #$08
__CP_BYTE_BIT:
    JSR __CP_BIT
    ROL __CP_STATE+6
    DEX
    BNE __CP_BYTE_BIT
    LDA __CP_STATE+6
    RTS
";

#[cfg(test)]
mod tests {
    use super::*;
    const RAW: &str = "__STR_PRINT_PTR:
    LDY #$00
    LDA ($FD),Y
    BEQ __RAW_DONE
    TAX
    INY
__RAW_LOOP:
    LDA ($FD),Y
    JSR $FFD2
    INY
    DEX
    BNE __RAW_LOOP
__RAW_DONE:
    RTS
";

    fn fixture(records: &[Vec<u8>], extra: &str, bss: bool) -> String {
        let mut s = "*=$2000\n    JMP $FFFE\n__STR_PRINT_I:\n    RTS\n".to_owned();
        for i in 0..records.len() {
            writeln!(s, "    JSR __STR_PRINT_I\n    .word S{i}").unwrap();
        }
        s.push_str(extra);
        s.push('\n');
        s.push_str(POOL);
        s.push('\n');
        for (i, bytes) in records.iter().enumerate() {
            writeln!(s, "S{i}:").unwrap();
            let mut record = vec![bytes.len() as u8];
            record.extend(bytes);
            byte_line(&mut s, &record);
        }
        s.push_str("; --- runtime ---\n");
        s.push_str("__STR_PRINT:\n    STA $FD\n    STY $FE\n");
        s.push_str(RAW);
        if bss {
            s.push_str("__BSS_START:\n    .byte $00\n__BSS_END:\n");
        }
        s.push_str("__HEAP_BOTTOM:\n");
        s
    }

    fn corpus() -> Vec<Vec<u8>> {
        let mut records: Vec<_> = (0..=255)
            .map(|b| {
                let mut v = vec![65, 66];
                v.extend([b; 62]);
                v
            })
            .collect();
        records.extend([vec![0; 255], vec![255; 255], vec![], vec![33]]);

        records.push(
            (0..255)
                .map(|i| if i % 7 == 0 { 0 } else { 65 + (i % 3) as u8 })
                .collect(),
        );
        records
    }

    struct Image {
        sys: emu64::System,
        asm: asm6502::Assembler6502,
        len: usize,
        expected_decimal: Option<bool>,
        pointer: usize,
    }
    fn image(source: &str) -> Image {
        let mut asm = asm6502::Assembler6502::new();
        asm.set_origin(0x2000);
        let bytes = asm.assemble_bytes(source).unwrap();
        let mut sys = emu64::System::new();
        sys.mem.ram[0x2000..0x2000 + bytes.len()].copy_from_slice(&bytes);
        Image {
            sys,
            asm,
            len: bytes.len(),
            expected_decimal: None,
            pointer: 0xfd,
        }
    }
    fn push(sys: &mut emu64::System, b: u8) {
        sys.mem.ram[0x100 + usize::from(sys.cpu.regs.sp)] = b;
        sys.cpu.regs.sp = sys.cpu.regs.sp.wrapping_sub(1);
    }
    fn ret(sys: &mut emu64::System) {
        sys.cpu.regs.sp = sys.cpu.regs.sp.wrapping_add(1);
        let lo = sys.mem.ram[0x100 + usize::from(sys.cpu.regs.sp)];
        sys.cpu.regs.sp = sys.cpu.regs.sp.wrapping_add(1);
        let hi = sys.mem.ram[0x100 + usize::from(sys.cpu.regs.sp)];
        sys.cpu.regs.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
    }

    fn invoke(img: &mut Image, record: usize, nested: bool, chars: &mut Vec<u8>) {
        let ptr = img.asm.lookup(&format!("S{record}")).unwrap() as u16;
        let pair_range = img.pointer..img.pointer + 2;
        img.sys.mem.ram[pair_range.clone()].copy_from_slice(&ptr.to_le_bytes());
        img.sys.cpu.regs.pc = img.asm.lookup("__STR_PRINT_PTR").unwrap() as u16;
        let before_sp = img.sys.cpu.regs.sp;
        push(&mut img.sys, 0xff);
        push(&mut img.sys, 0xfd);
        let mut hook = nested;
        for _ in 0..200_000 {
            if img.sys.cpu.regs.pc == 0xfffe {
                break;
            }
            if img.sys.cpu.regs.pc == 0xffd2 {
                if let Some(expected) = img.expected_decimal {
                    assert_eq!(
                        img.sys.cpu.regs.p & 8 != 0, expected,
                        "CHROUT's D flag changed"
                    );
                }
                if hook {
                    hook = false;
                    let regs = img.sys.cpu.regs;
                    let pair = [img.sys.mem.ram[img.pointer], img.sys.mem.ram[img.pointer + 1]];

                    invoke(img, 256, false, chars);
                    img.sys.cpu.regs = regs;
                    img.sys.mem.ram[pair_range.clone()].copy_from_slice(&pair);
                }
                let byte = img.sys.cpu.regs.a;
                chars.push(byte);
                // Adversarial CHROUT return values: A need not equal input, and both C/V values are observable at the caller's return.
                img.sys.cpu.regs.a ^= 0x55;
                img.sys.cpu.regs.p = (img.sys.cpu.regs.p & !0x41) | (byte & 1) | ((byte & 2) << 5);
                ret(&mut img.sys);
            } else {
                assert!(img.sys.cpu.step(&mut img.sys.mem));
            }
        }
        assert_eq!(
            img.sys.cpu.regs.pc, 0xfffe,
            "record {record} did not return"
        );
        assert_eq!(
            img.sys.cpu.regs.sp, before_sp,
            "record {record} leaked stack"
        );
        assert_eq!(&img.sys.mem.ram[pair_range], &ptr.to_le_bytes());
    }

    #[test]
    fn register_prints_pack_and_other_uses_stay_raw() {
        let site = |i: usize| format!("    JSR __STR_PRINT_I\n    .word S{i}\n");
        let helper = "__SEQ_9:\n    JSR __STR_PRINT\n    RTS\n__STR_ASSIGN:\n    RTS\n";
        let source = fixture(&corpus(), helper, false)
            .replacen(&site(0), "    LDA #<S0\n    LDY #>S0\n    JSR __STR_PRINT\n", 1)
            .replacen(&site(1), "    LDY #>S1\n    LDA #<S1\n__TAIL_1:\n    JMP __SEQ_9\n", 1)
            .replacen(&site(2), "    LDA #<S2\n    LDY #>S2\n    JSR __STR_ASSIGN\n", 1)
            .replacen(&site(3), "    LDA #<S3\n    LDY #>S3\n    JSR __STR_PRINT\n    LDA #<S3\n", 1);
        let packed = compress(&source).expect("profitable");
        let (begin, end) = (packed.find("__CP_BEGIN:").unwrap(), packed.find("__CP_END:").unwrap());
        let at = |name: &str| packed.find(&format!("\n{name}:\n")).unwrap();
        for (name, packs) in [("S0", true), ("S1", true), ("S2", false), ("S3", false), ("S4", true)] {
            assert_eq!((begin..end).contains(&at(name)), packs, "{name}");
        }
    }

    #[test]
    fn relocated_print_pointer_decodes_both_codecs_and_preserves_nested_abi() {

        let mut record: Vec<u8> = (1..=127).collect();
        record.extend([65; 128]);
        let mut records = vec![record; 257];
        records[254] = vec![];
        records[255] = vec![33];
        // FD/FE remains an unrelated pointer.
        for lo in [0x02u8, 0x5a, 0xa3, 0xfc, 0xfd, 0xfe] {
            let source = fixture(&records, "", false)
                .replace("$FD", "@LOW@")
                .replace("$FE", &format!("${:02X}", lo + 1))
                .replace("@LOW@", &format!("${lo:02X}"));
            for encode in [compress, compress_compact] {
                let packed = encode(&source).expect("profitable relocated codec");
                let mut img = image(&packed);
                img.pointer = usize::from(lo);
                img.sys.cpu.regs.sp = 0xd0;
                for (index, nested) in [(0, false), (254, false), (255, false), (0, true)] {
                    if lo < 0xfc {
                        let unrelated = img.asm.lookup("S254").unwrap();
                        img.sys.mem.ram[0xfd..0xff].copy_from_slice(&unrelated.to_le_bytes());
                    }
                    let mut chars = Vec::new();
                    invoke(&mut img, index, nested, &mut chars);
                    let expected = if nested {
                        [records[256].as_slice(), records[index].as_slice()].concat()
                    } else {
                        records[index].clone()
                    };
                    assert_eq!(chars, expected, "pointer ${lo:02X}, record {index}");
                }
            }
        }
    }

    #[test]
    fn print_pointer_recognition_accepts_inline_only_and_declines_unknown_abis() {
        let source = fixture(&corpus(), "", false);
        let prologue = "__STR_PRINT:\n    STA $FD\n    STY $FE\n";
        let pointer_only = source.replace(prologue, "");
        assert!(compress(&pointer_only).is_some(), "the register entry is optional");
        let spaced = source.replace("STA $FD", "STA   $FD ; pointer low")
            .replace("STY $FE", "STY\t$FE")
            .replace("LDY #$00", "LDY  #$00");
        assert!(compress(&spaced).is_some());
        for malformed in [
            source.replace("STY $FE", "STY $FC"),
            source.replace("STA $FD", "STX $FD"),
            source.replace("LDA ($FD),Y", "LDA ($FC),Y"),
            source.replace("LDY #$00", "LDY #$01"),
            pointer_only.replace("($FD)", "($FF)"),
            pointer_only.replace("($FD)", "($0100)"),
            source.clone() + "\n__STR_PRINT_PTR:\n",
            source.clone() + "\n__STR_PRINT:\n",
        ] {
            assert!(compress(&malformed).is_none(), "unknown pointer ABI must decline");
            assert!(canonical_fixture(&malformed).is_none());
        }
    }

    fn canonical_fixture(source: &str) -> Option<String> {
        let Pool {
            lines,
            start,
            end,
            records,
            eligible,
        } = parse_pool(source)?;
        let model = compact_model(&records, &eligible, None)?;
        let decoder = DECODER.replacen(TREE_NODE, CANONICAL_NODE, 1);
        emit_pool(
            &lines,
            start,
            end,
            &model.records,
            &[("COUNTS", &model.counts), ("VALUES", &model.values)],
            &decoder,
        )
    }

    #[test]
    fn assembled_decoder_preserves_every_byte_length_register_and_nested_state() {
        let records = corpus();
        let source = fixture(&records, "", true);
        for encode in [compress, canonical_fixture] {
            let packed = encode(&source).unwrap();
            assert!(packed.find("__CP_PACKED:").unwrap() < packed.find("__BSS_START:").unwrap());
            assert_eq!(compress(&packed), None, "a second pass must decline");
            let mut a = image(&source);
            let mut b = image(&packed);
            assert!(b.len < a.len);
            for (nested, flags) in [(false, 0x65), (true, 0x65), (false, 0x6D), (true, 0x6D)] {
                for (i, expected) in records.iter().enumerate() {
                    for img in [&mut a, &mut b] {
                        img.sys.cpu.regs.a = 0xa5;
                        img.sys.cpu.regs.x = 37;
                        img.sys.cpu.regs.y = 91;
                        img.sys.cpu.regs.p = flags;
                        img.sys.cpu.regs.sp = 0xd0;
                    }
                    let (mut raw, mut decoded) = (Vec::new(), Vec::new());
                    invoke(&mut a, i, nested, &mut raw);
                    invoke(&mut b, i, nested, &mut decoded);
                    assert_eq!(raw, decoded, "record {i}, nested {nested}");
                    if !nested {
                        assert_eq!(&decoded, expected);
                    }
                    let x = a.sys.cpu.regs;
                    let y = b.sys.cpu.regs;
                    assert_eq!(
                        (x.a, x.x, x.y, x.p),
                        (y.a, y.x, y.y, y.p),
                        "record {i}, nested {nested}"
                    );
                }
            }
        }
    }

    fn text_corpus() -> Vec<Vec<u8>> {
        let words = [
            "THE", "YOU", "ARE", "IN", "A", "DARK", "ROOM", "THERE", "IS", "DOOR", "TO", "NORTH",
            "SOUTH", "AND", "AN", "OLD", "LAMP", "HERE", "CAN", "SEE", "PASSAGE", "LEADS", "WEST",
        ];
        let mut seed = 12345u32;
        (0..260)
            .map(|_| {
                let mut text = String::new();
                for _ in 0..8 + seed as usize % 9 {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    text.push_str(words[(seed >> 16) as usize % words.len()]);
                    text.push(' ');
                }
                text.push('.');
                text.into_bytes()
            })
            .collect()
    }

    #[test]
    fn pair_decoder_matches_the_raw_print() {
        for (records, pairs) in [(text_corpus(), true), (corpus(), false)] {
            let source = fixture(&records, "", true);
            let packed = compress_size(&source, false).unwrap();
            assert_eq!(packed.contains("__CP_PAIRL:"), pairs);
            let mut a = image(&source);
            let mut b = image(&packed);
            assert!(b.len < a.len);
            for (nested, flags) in [(false, 0x65), (true, 0x65), (false, 0x6D), (true, 0x6D)] {
                for (i, expected) in records.iter().enumerate() {
                    for img in [&mut a, &mut b] {
                        img.sys.cpu.regs.a = 0xa5;
                        img.sys.cpu.regs.x = 37;
                        img.sys.cpu.regs.y = 91;
                        img.sys.cpu.regs.p = flags;
                        img.sys.cpu.regs.sp = 0xd0;
                    }
                    let (mut raw, mut decoded) = (Vec::new(), Vec::new());
                    invoke(&mut a, i, nested, &mut raw);
                    invoke(&mut b, i, nested, &mut decoded);
                    assert_eq!(raw, decoded, "record {i}, nested {nested}");
                    if !nested {
                        assert_eq!(&decoded, expected);
                    }
                    let (x, y) = (a.sys.cpu.regs, b.sys.cpu.regs);
                    assert_eq!((x.a, x.x, x.y, x.p), (y.a, y.x, y.y, y.p), "record {i}");
                }
            }
        }
    }

    #[test]
    fn pairs_shrink_text_below_the_plain_tree_code() {
        let source = fixture(&text_corpus(), "", false);
        let plain = image(&compress(&source).unwrap()).len;
        let paired = image(&compress_size(&source, false).unwrap()).len;
        assert!(paired + 200 < plain, "{paired} vs {plain}");
    }

    #[test]
    fn decoder_prices_are_the_assembled_sizes() {
        let size = |decoder: &str| {
            let src = format!(
                "*=$2000\n{}{}__CP_LEFT:\n__CP_RIGHT:\n__CP_VALUES:\n__CP_COUNTS:\n__CP_PAIRL:\n\
                 __CP_PAIRR:\n__CP_BEGIN:\n__CP_END:\n__CP_STATE:\n__CP_RET:\n",
                bind_pointer(DISPATCH, "$FD", "$FE"),
                bind_pointer(decoder, "$FD", "$FE")
            );
            asm6502::Assembler6502::new().assemble_bytes(&src).unwrap().len() + 8
        };
        assert_eq!(size(DECODER), TREE_DECODER_COST);
        assert_eq!(size(&DECODER.replacen(TREE_NODE, CANONICAL_NODE, 1)), CANONICAL_DECODER_COST);
        assert_eq!(size(&pair_decoder(40)) - size(DECODER), PAIR_DECODER_COST);
    }

    #[test]
    fn all_128_huffman_leaves_decode_across_byte_boundaries() {
        // 127 different literal symbols plus the run marker fill the leaf namespace exactly.
        let mut record: Vec<u8> = (1..=127).collect();
        record.extend([65; 128]);
        let records = vec![record.clone(); 20];
        let source = fixture(&records, "", false);
        for encode in [compress, compress_compact] {
            let packed = encode(&source).unwrap();
            let mut img = image(&packed);
            img.sys.cpu.regs.sp = 0xd0;
            let mut chars = Vec::new();
            for i in 0..records.len() {
                invoke(&mut img, i, false, &mut chars);
            }
            assert_eq!(chars, records.concat());
        }
    }

    #[test]
    fn canonical_decoder_preserves_decimal_mode_and_every_entry_flag() {
        // Enough leaves to make canonical subtraction/addition cross BCD digit boundaries.
        let mut record: Vec<u8> = (1..=127).collect();
        record.extend([65; 128]);
        let source = fixture(&vec![record.clone(); 20], "", false);
        let mut before = image(&compress(&source).unwrap());
        let mut after = image(&compress_compact(&source).unwrap());
        for flags in 0..=255 {
            for img in [&mut before, &mut after] {
                img.sys.cpu.regs.a = 0xA5;
                img.sys.cpu.regs.x = 37;
                img.sys.cpu.regs.y = 91;
                img.sys.cpu.regs.p = flags;
                img.sys.cpu.regs.sp = 0xD0;
                img.expected_decimal = Some(flags & 8 != 0);
            }
            let (mut raw, mut decoded) = (Vec::new(), Vec::new());
            invoke(&mut before, 0, false, &mut raw);
            invoke(&mut after, 0, false, &mut decoded);
            assert_eq!(raw, record, "tree decoder, P=${flags:02X}");
            assert_eq!(decoded, raw, "canonical decoder, P=${flags:02X}");
            let a = before.sys.cpu.regs;
            let b = after.sys.cpu.regs;
            assert_eq!(
                (a.a, a.x, a.y, a.p, a.sp),
                (b.a, b.x, b.y, b.p, b.sp),
                "P=${flags:02X}"
            );
            assert_eq!(b.p & 8, flags & 8, "decimal flag was lost");
        }
    }

    #[test]
    fn complete_reference_set_excludes_shared_and_indirect_records() {
        for encode in [compress, canonical_fixture] {
            for extra in [
                "    LDA #<S0",
                "    LDY #>S0",
                "    .word S0",
                "    JSR __SYS_TXT_OUT\n    .word S0",
                "    STA S0+1",
                "    .word S0,S1",
            ] {
                let packed = encode(&fixture(&corpus(), extra, false)).unwrap();
                let end = packed.find("__CP_END:").unwrap();
                assert!(
                    packed.find("\nS0:\n").unwrap() > end,
                    "shared record compressed: {extra}"
                );
            }
            let harmless = encode(&fixture(&corpus(), "; .word S0", false)).unwrap();
            assert!(harmless.find("\nS0:\n").unwrap() < harmless.find("__CP_END:").unwrap());
        }
    }

    #[test]
    fn val_sentinel_is_atomic_through_packing_and_banking() {
        let mut record: Vec<u8> = (1..=127).collect();
        record.extend([65; 128]);
        let records = vec![record; 20];
        let mut source = fixture(&records, "    LDA #<S0\n    LDY #>S0", false);
        let mut ordinary = String::from("S0:\n");
        let mut bytes = vec![records[0].len() as u8];
        bytes.extend(&records[0]);
        byte_line(&mut ordinary, &bytes);
        bytes.push(0);
        let mut guarded = String::from("S0:\n; VAL literal sentinel\n");
        byte_line(&mut guarded, &bytes);
        source = source.replacen(&ordinary, &guarded, 1);
        assert!(parse_pool(&source).is_some());
        for encode in [compress, compress_compact] {
            let packed = encode(&source).expect("guard must not disable profitable PRINT packing");
            assert!(packed.contains("S0:\n; VAL literal sentinel"));
            for source in [packed.clone(), crate::extraram::inject(&packed)] {
                let linked = image(&source);
                let start = linked.asm.lookup("S0").unwrap();
                assert_eq!(&linked.sys.mem.ram[usize::from(start)..usize::from(start) + bytes.len()], bytes);
            }
        }
        // The explicit marker is part of the private format; malformed padding must never be accepted as an ordinary compressible string.
        assert!(parse_pool(&source.replace("; VAL literal sentinel", "")).is_none());
        assert!(parse_pool(&source.replace(",$00\nS1:", ",$01\nS1:")).is_none());
    }

    #[test]
    fn unsupported_or_unprofitable_inputs_decline_without_panicking() {
        for records in [
            vec![vec![]],
            vec![vec![65; 255]],
            vec![(0..=255).collect()],
            vec![vec![65, 66, 67]],
            vec![(1..=128).collect(), (129..=255).collect()],
        ] {
            assert!(compress(&fixture(&records, "", false)).is_none());
        }
        let source = fixture(&corpus(), "", false);
        assert!(compress(&source.replace("S0:", "UNEXPECTED:")).is_none());
        assert!(compress(&source.replace("__HEAP_BOTTOM:", "")).is_some());
        assert!(compress(&source.replace("__STR_PRINT_PTR:", "OTHER_PRINT:")).is_none());
    }

    #[test]
    fn conservative_code_price_covers_near_and_long_dispatch_branches() {
        for (decoder, bound) in [
            (DECODER.to_owned(), CODE_COST_BOUND),
            (
                DECODER.replacen(TREE_NODE, CANONICAL_NODE, 1),
                COMPACT_CODE_COST_BOUND,
            ),
        ] {
            for padding in [0, 512] {
                let middle = "    .byte $00\n".repeat(padding);
                let source = format!(
                "*=$2000\n__CP_BEGIN:\n    .byte $00\n__CP_END:\n{DISPATCH}{RAW}\n{middle}{decoder}"
            )
            .replacen("__STR_PRINT_PTR:\n", "", 1);
                let mut asm = asm6502::Assembler6502::new();
                let mut source = bind_pointer(&source, "$FD", "$FE");
                for name in ["LEFT", "RIGHT", "VALUES", "COUNTS"] {
                    writeln!(source, "__CP_{name}:\n    .byte $00").unwrap();
                }
                source.push_str("__CP_STATE:\n    .byte $00,$00,$00,$00,$00,$00,$00\n__CP_RET:\n    .byte $00\n");
                let total = asm.assemble_bytes(&source).unwrap().len();
                let mut raw = asm6502::Assembler6502::new();
                let raw_len = raw.assemble_bytes(RAW).unwrap().len();
                // The bound covers the eight state bytes, not the four one-byte tables.
                let overhead = total - raw_len - padding - 1 - 4;
                assert!(overhead <= bound, "actual dispatch/decoder cost {overhead}");
            }
        }
    }

    #[test]
    fn adaptive_runs_use_code_lengths_and_keep_nul_escaped() {
        let lengths = BTreeMap::from([(0, 3), (b'A', 2), (b'B', 8)]);
        assert_eq!(
            adaptive_tokens(b"AAAABBBB\0", &lengths),
            vec![
                (b'A', None),
                (b'A', None),
                (b'A', None),
                (b'A', None),
                (0, Some((4, b'B'))),
                (0, Some((1, 0))),
            ]
        );
    }

    #[test]
    fn compact_supports_profitable_single_symbol_without_zero_bit_codes() {
        let records: Vec<_> = (32..=255).map(|n| vec![b'A'; n]).collect();
        let source = fixture(&records, "", false);
        assert!(compress(&source).is_none());
        let packed = compress_compact(&source).unwrap();
        let mut img = image(&packed);
        img.sys.cpu.regs.sp = 0xd0;
        let mut chars = Vec::new();
        for i in 0..records.len() {
            invoke(&mut img, i, false, &mut chars);
        }
        assert_eq!(chars, records.concat());
    }

    #[test]
    fn compact_declines_when_extra_decoder_cost_is_not_repaid() {
        assert!(compress(&fixture(&corpus(), "", false)).is_some());
        assert!(compress_compact(&fixture(&corpus(), "", false)).is_none());
        assert!(compress_compact(&fixture(&[(0..=255).collect()], "", false)).is_none());
    }
}
