//! `cargo xtask wide-stores <image>`: lists the title's 256-bit vector stores
//! for `VICTORY_PATCH_WIDE` (`victory-loader/src/wide.rs`).
//!
//! Ported from pragyaangaur/VIctory-macOS (GPL-3.0-or-later, the same
//! upstream as ours), `xtask/src/wide_stores.rs` of its PR #262; split here
//! into the disassembly read (`scan`) and the site planning (`plan`) so both
//! can be tested without a title image.
//!
//! Rosetta 2 stops ("unexpectedly need to EmulateForward on a synchronous
//! exception") when a 32-byte store begins in writable memory and faults in
//! the next page, which fault tracking makes happen (docs/macos.md, "Plain
//! 256-bit stores into protected pages stop Rosetta too"). The loader
//! rewrites each listed store as a jump to a trampoline that stores the two
//! 128-bit halves. This finds the stores with `llvm-objdump` and prints the
//! list; nothing of the title is in it but offsets, lengths and condition
//! codes.
//!
//! The image is the mapped copy VIctory writes to its cache
//! (`<cache_dir>/images/victory-image-<16 hex digits>.dll`), whose code
//! bytes are the title's.
//!
//! A store of 5 bytes or more becomes a 5-byte jump in place. A 4-byte store
//! has no room for one: its run takes in the instructions after it (or
//! before it) that can be moved, until 5 bytes are covered, and only where
//! no branch lands inside the run and no function (`.pdata`) begins or ends
//! in it. A line: RVA (hex), bytes replaced, then the pieces of the run: `sN`
//! a 256-bit store, `cN` bytes copied, `rN:K:T` bytes copied with their
//! RIP-relative displacement at K redone for RVA T, `kN:CC:T` a conditional
//! jump to T, `jN:T` a jump to T that ends the run.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// One decoded instruction: its RVA, length and text (`mnemonic operands`,
/// with any `# 0x...` comment `llvm-objdump` adds).
type Insn = (u32, usize, String);

struct Store {
    insn: Insn,
    /// The three instructions before it, nearest first.
    before: Vec<Insn>,
    /// The four after it.
    after: Vec<Insn>,
}

/// A section: (virtual address, virtual size, raw offset, raw size).
type Section = (u32, u32, u32, u32);

struct Sections {
    base: u64,
    by_name: HashMap<String, Section>,
}

fn sections(data: &[u8]) -> Result<Sections> {
    anyhow::ensure!(data.len() > 0x40 && &data[..2] == b"MZ", "not a PE image");
    let u16_at = |at: usize| -> Result<usize> {
        let b = data.get(at..at + 2).context("PE headers cut short")?;
        Ok(u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32_at = |at: usize| -> Result<u32> {
        let b = data.get(at..at + 4).context("PE headers cut short")?;
        Ok(u32::from_le_bytes(b.try_into()?))
    };
    let pe = u32_at(0x3c)? as usize;
    anyhow::ensure!(data.get(pe..pe + 4) == Some(b"PE\0\0"), "not a PE image");
    let count = u16_at(pe + 6)?;
    let optional = u16_at(pe + 20)?;
    anyhow::ensure!(u16_at(pe + 24)? == 0x20b, "not a 64-bit (PE32+) image");
    let base = u64::from_le_bytes(
        data.get(pe + 48..pe + 56)
            .context("PE headers cut short")?
            .try_into()?,
    );
    let mut by_name = HashMap::new();
    for i in 0..count {
        let at = pe + 24 + optional + 40 * i;
        let name = data.get(at..at + 8).context("section table cut short")?;
        let name = String::from_utf8_lossy(name)
            .trim_end_matches('\0')
            .to_string();
        by_name.insert(
            name,
            (
                u32_at(at + 12)?,
                u32_at(at + 8)?,
                u32_at(at + 20)?,
                u32_at(at + 16)?,
            ),
        );
    }
    Ok(Sections { base, by_name })
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.starts_with(word)
        && !text[word.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A 256-bit store to memory: `v<op> ymmword ptr [...], ymmN`.
fn is_store(text: &str) -> bool {
    let Some((mnemonic, rest)) = text.split_once(' ') else {
        return false;
    };
    let Some(inside) = rest.strip_prefix("ymmword ptr [") else {
        return false;
    };
    let Some((address, register)) = inside.split_once("], ") else {
        return false;
    };
    mnemonic.starts_with('v')
        && !address.contains(']')
        && register
            .strip_prefix("ymm")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_masked(text: &str) -> bool {
    text.starts_with("vmaskmov") || text.starts_with("vpmaskmov")
}

/// What cannot be moved out of place (or is patched by the masked-store patch).
fn is_fixed(text: &str) -> bool {
    const PREFIXES: [&str; 19] = [
        "vextract",
        "vcvtps2ph",
        "vpmaskmov",
        "vmaskmov",
        "call",
        "ret",
        "loop",
        "bnd",
        "int3",
        "syscall",
        "xbegin",
        "xend",
        "push",
        "pop",
        "enter",
        "leave",
        "rep",
        "lock",
        "j",
    ];
    PREFIXES
        .iter()
        .any(|p| text.starts_with(p) && (*p != "j" || text.len() > 1))
        || starts_with_word(text, "int")
}

/// The hexadecimal number at the start of `text` after `0x`.
fn hex_after_0x(text: &str) -> Option<u64> {
    let digits = text.strip_prefix("0x")?;
    let end = digits
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(digits.len());
    u64::from_str_radix(&digits[..end], 16).ok()
}

/// `(mnemonic, target)` of a direct branch.
fn branch(text: &str) -> Option<(&str, u64)> {
    let (mnemonic, rest) = text.split_once(' ')?;
    let is_branch = (mnemonic.starts_with('j') && mnemonic.len() > 1)
        || mnemonic == "call"
        || mnemonic.starts_with("loop")
        || mnemonic == "xbegin";
    if !is_branch {
        return None;
    }
    Some((mnemonic, hex_after_0x(rest)?))
}

/// The target of the RIP-relative operand `llvm-objdump` notes as `# 0x...`.
fn noted_target(text: &str) -> Option<u64> {
    hex_after_0x(&text[text.find("# 0x")? + 2..])
}

/// `n` bytes of the file at `rva`, in whichever section holds them.
fn at_rva<'a>(data: &'a [u8], secs: &Sections, rva: u32, n: usize) -> Option<&'a [u8]> {
    secs.by_name.values().find_map(|&(va, vsize, raw, rsize)| {
        let off = rva.checked_sub(va)?;
        if off >= vsize.max(rsize) || off as usize + n > rsize as usize {
            return None;
        }
        data.get((raw + off) as usize..(raw + off) as usize + n)
    })
}

/// The functions `.pdata` lists: (begin, end) RVAs, sorted by begin.
fn functions(data: &[u8], secs: &Sections) -> Vec<(u32, u32)> {
    let Some(&(_, vsize, raw, rsize)) = secs.by_name.get(".pdata") else {
        return Vec::new();
    };
    let len = vsize.min(rsize) as usize;
    let table = data
        .get(raw as usize..)
        .map(|d| &d[..len.min(d.len())])
        .unwrap_or_default();
    let mut out: Vec<(u32, u32)> = table
        .chunks_exact(12)
        .map(|e| {
            let w = |k: usize| u32::from_le_bytes([e[k], e[k + 1], e[k + 2], e[k + 3]]);
            (w(0), w(4))
        })
        .filter(|&(b, e)| b < e)
        .collect();
    out.sort_unstable();
    out
}

/// Where branches may land besides direct targets: function starts and ends
/// (`.pdata`), anything in the data that looks like an address in the code
/// (switch tables, catch continuations, function pointers), and every 64-bit
/// address `.reloc` names that points into the code.
fn data_targets(data: &[u8], secs: &Sections, text: Section) -> HashSet<u32> {
    let (tva, tsize, _, _) = text;
    let in_text = |v: u32| (tva..tva + tsize).contains(&v);
    let mut targets = HashSet::new();
    for (b, e) in functions(data, secs) {
        targets.insert(b);
        targets.insert(e);
    }
    let raw = |(_, _, raw, rsize): Section| {
        data.get(raw as usize..)
            .map(|d| &d[..(rsize as usize).min(d.len())])
            .unwrap_or_default()
    };
    for (name, &section) in &secs.by_name {
        if name == ".text" || name == ".pdata" || name == ".reloc" {
            continue;
        }
        for word in raw(section).chunks_exact(4) {
            let value = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            if in_text(value) {
                targets.insert(value);
            }
        }
    }
    // IMAGE_REL_BASED_DIR64 entries: the 64-bit address they fix up.
    if let Some(&reloc) = secs.by_name.get(".reloc") {
        let blocks = raw(reloc);
        let mut at = 0;
        while let Some(head) = blocks.get(at..at + 8) {
            let page = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
            let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            if len < 8 {
                break;
            }
            for e in blocks
                .get(at + 8..at + len)
                .unwrap_or_default()
                .chunks_exact(2)
            {
                let e = u16::from_le_bytes([e[0], e[1]]);
                if e >> 12 != 10 {
                    continue;
                }
                let Some(b) = at_rva(data, secs, page + u32::from(e & 0xfff), 8) else {
                    continue;
                };
                let value = u64::from_le_bytes(b.try_into().expect("8 bytes"));
                if let Some(rva) = value
                    .checked_sub(secs.base)
                    .and_then(|v| u32::try_from(v).ok())
                    && in_text(rva)
                {
                    targets.insert(rva);
                }
            }
            at += len;
        }
    }
    targets
}

/// What [`scan`] found in a disassembly.
#[derive(Default)]
struct Scan {
    stores: Vec<Store>,
    /// (instruction, table) of loads `[r + 4*r + disp]` whose displacement
    /// is an RVA in `.text`: switch tables of 4-byte RVAs (MSVC x64).
    dword_tables: Vec<(u32, u32)>,
    /// (instruction, table) of `movzx r, byte ptr [r + r + disp]` the same
    /// way: the index tables of two-level switches.
    byte_tables: Vec<(u32, u32)>,
    /// RVAs in `.text` that `lea r, [rip + X]` names.
    lea_text: Vec<u32>,
    /// Function begins no instruction of the listing starts at: the listing
    /// is out of step with the code there.
    unsynced: HashSet<u32>,
}

/// The address and byte count of a listing line, whatever its instruction.
fn span(line: &str, base: u64) -> Option<(u32, usize)> {
    let (address, rest) = line.split_once(':')?;
    let address = u64::from_str_radix(address.trim(), 16).ok()?;
    let bytes = rest.split('\t').next()?.split_whitespace().count();
    (bytes > 0).then_some((u32::try_from(address.checked_sub(base)?).ok()?, bytes))
}

/// The displacement of the memory operand of `t` (`... ptr [a + b + 0x...]`)
/// and its terms, for a load that is not RIP-relative.
fn displacement(t: &str) -> Option<(u64, Vec<&str>)> {
    let inside = t.split_once("ptr [")?.1.split_once(']')?.0;
    if inside.contains("rip") {
        return None;
    }
    let terms: Vec<&str> = inside.split(" + ").collect();
    Some((hex_after_0x(terms.last()?)?, terms))
}

/// Every 256-bit store of a disassembly, with the three instructions before
/// it and the four after; every direct branch's target and every RVA in
/// `.text` (`text`: start, size) a `lea r, [rip + X]` names is added to
/// `targets`. `begins`: the functions' begins, sorted.
fn scan(
    reader: impl BufRead,
    base: u64,
    text: (u32, u32),
    begins: &[u32],
    targets: &mut HashSet<u32>,
) -> Result<Scan> {
    let in_text = |v: u64| (u64::from(text.0)..u64::from(text.0) + u64::from(text.1)).contains(&v);
    let mut out = Scan::default();
    let mut recent: VecDeque<Insn> = VecDeque::with_capacity(4);
    let mut waiting: Vec<usize> = Vec::new();
    let mut next_begin = 0;
    for line in reader.lines() {
        let line = line?;
        if let Some((rva, n)) = span(&line, base) {
            while let Some(&b) = begins.get(next_begin)
                && b < rva + n as u32
            {
                if b != rva {
                    out.unsynced.insert(b);
                }
                next_begin += 1;
            }
        }
        let Some(insn) = parse(&line, base) else {
            // A label or a gap: what came before is not the store's neighbour.
            if line.trim().is_empty() || line.ends_with(':') {
                recent.clear();
                waiting.clear();
            }
            continue;
        };
        let t = command(&insn.2);
        if let Some((_, target)) = branch(&insn.2) {
            targets.insert(target.wrapping_sub(base) as u32);
        }
        if t.starts_with("lea ")
            && t.contains("rip")
            && let Some(to) = noted_target(&insn.2).and_then(|v| v.checked_sub(base))
            && in_text(to)
        {
            targets.insert(to as u32);
            out.lea_text.push(to as u32);
        }
        if let Some((disp, terms)) = displacement(t)
            && in_text(disp)
        {
            let registers = terms.len() == 3 && !terms[0].contains('*');
            if (t.starts_with("mov ") || t.starts_with("movsxd "))
                && t.contains("dword ptr")
                && registers
                && terms[1].starts_with("4*")
            {
                out.dword_tables.push((insn.0, disp as u32));
            } else if t.starts_with("movzx ")
                && t.contains("byte ptr")
                && registers
                && !terms[1].contains('*')
            {
                out.byte_tables.push((insn.0, disp as u32));
            }
        }
        for &w in &waiting {
            out.stores[w].after.push(insn.clone());
        }
        waiting.retain(|&w| out.stores[w].after.len() < 4);
        if is_store(t) {
            out.stores.push(Store {
                insn: insn.clone(),
                before: recent.iter().rev().cloned().collect(),
                after: Vec::new(),
            });
            waiting.push(out.stores.len() - 1);
        }
        if recent.len() == 3 {
            recent.pop_front();
        }
        recent.push_back(insn);
    }
    // Begins past the listing's end were never reached.
    out.unsynced
        .extend(begins.get(next_begin..).unwrap_or_default());
    Ok(out)
}

/// The function of `functions` (sorted (begin, end)) that holds `rva`.
fn function_of(functions: &[(u32, u32)], rva: u32) -> Option<(u32, u32)> {
    let i = functions
        .partition_point(|&(b, _)| b <= rva)
        .checked_sub(1)?;
    let f = functions[i];
    (rva < f.1).then_some(f)
}

/// The stores of `scan` that can be listed, and why the others cannot.
struct Kept {
    stores: Vec<Store>,
    /// Byte spans of the code that hold switch tables, sorted.
    tables: Vec<(u32, u32)>,
    /// Stores outside every `.pdata` function (or across its end).
    outside: usize,
    /// Stores in a function the listing is out of step with.
    unsynced: usize,
    /// Stores in a switch table, or after one in its function.
    in_tables: usize,
}

/// Keeps the stores of `scan` that lie wholly inside one `.pdata` function
/// the listing is in step with, outside its switch tables and before them;
/// each table entry becomes a branch target. `code(rva, n)`: `.text`'s bytes.
fn keep(
    scan: Scan,
    functions: &[(u32, u32)],
    text: (u32, u32),
    code: &dyn Fn(u32, usize) -> Option<Vec<u8>>,
    targets: &mut HashSet<u32>,
) -> Kept {
    let in_text = |v: u32| (text.0..text.0 + text.1).contains(&v);
    let is_begin = |rva: u32| functions.binary_search_by_key(&rva, |&(b, _)| b).is_ok();
    // 4-byte RVA tables: as many entries as point into the code, up to the
    // next function. Taken from a `lea` only with two entries or more, and
    // never at a function's begin (a function's address, not a table).
    let mut tables: Vec<(u32, u32)> = Vec::new();
    let mut with_table: HashSet<(u32, u32)> = HashSet::new();
    let leas = scan.lea_text.iter().map(|&t| (None, t, 2));
    let loads = scan.dword_tables.iter().map(|&(i, t)| (Some(i), t, 1));
    for (insn, start, least) in loads.chain(leas) {
        if is_begin(start) {
            continue;
        }
        let mut k = 0u32;
        while 4 * k < MAX_TABLE
            && !(k > 0 && is_begin(start + 4 * k))
            && let Some(w) = code(start + 4 * k, 4)
            && in_text(u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        {
            targets.insert(u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
            k += 1;
        }
        if k >= least {
            tables.push((start, start + 4 * k));
            if let Some(f) = insn.and_then(|i| function_of(functions, i)) {
                with_table.insert(f);
            }
        }
    }
    // Index tables of bytes, for a function that has a table of RVAs: up
    // to the next function begin (at most 64 KiB).
    for &(insn, start) in &scan.byte_tables {
        if function_of(functions, insn).is_some_and(|f| with_table.contains(&f)) {
            let i = functions.partition_point(|&(b, _)| b <= start);
            let end = functions
                .get(i)
                .map_or(start + 0x1_0000, |f| f.0.min(start + 0x1_0000));
            tables.push((start, end));
        }
    }
    tables.sort_unstable();
    let mut kept = Kept {
        stores: Vec::new(),
        tables: Vec::new(),
        outside: 0,
        unsynced: 0,
        in_tables: 0,
    };
    for store in scan.stores {
        let (rva, n) = (store.insn.0, store.insn.1 as u32);
        let Some(f) = function_of(functions, rva).filter(|f| rva + n <= f.1) else {
            kept.outside += 1;
            continue;
        };
        if scan.unsynced.contains(&f.0) {
            kept.unsynced += 1;
            continue;
        }
        // A table that begins before the store's end and ends after its
        // function's begin: the store is in it, or after it, where the
        // listing may have lost step.
        let i = tables.partition_point(|&(s, _)| s < rva + n);
        let near = tables[..i]
            .iter()
            .rev()
            .take_while(|&&(s, _)| s + MAX_TABLE >= f.0);
        if near.clone().any(|&(s, e)| e > rva || s >= f.0) {
            kept.in_tables += 1;
            continue;
        }
        kept.stores.push(store);
    }
    kept.tables = tables;
    kept
}

/// Whether `[lo, hi)` meets a span of `spans` (sorted by start).
fn meets(spans: &[(u32, u32)], lo: u32, hi: u32) -> bool {
    let i = spans.partition_point(|&(s, _)| s < hi);
    spans[..i]
        .iter()
        .rev()
        .take_while(|&&(s, _)| s + MAX_TABLE >= lo)
        .any(|&(_, e)| e > lo)
}

/// The longest table span [`keep`] makes: 65,536 RVAs.
const MAX_TABLE: u32 = 0x4_0000;

/// What [`plan`] found.
struct Plan {
    /// The list's lines, by RVA.
    lines: Vec<String>,
    /// 4-byte stores with no room and nothing movable beside them.
    unpatched: usize,
    /// 4-byte RIP-relative stores, left.
    rip_short: usize,
    /// Sites dropped for taking in bytes of a switch table.
    in_tables: usize,
}

/// The sites for `stores`. `code(rva, n)`: the image's `n` bytes at `rva`
/// (none outside `.text`).
fn plan(
    stores: &[Store],
    targets: &HashSet<u32>,
    tables: &[(u32, u32)],
    code: &dyn Fn(u32, usize) -> Option<Vec<u8>>,
    base: u64,
) -> Plan {
    let piece = |a: u32, m: usize, text: &str| -> Option<String> {
        let t = command(text);
        if is_store(t) {
            return (!t.contains("rip") && !is_masked(t)).then(|| format!("s{m}"));
        }
        if let Some((mnemonic, target)) = branch(t)
            && mnemonic.starts_with('j')
            && !mnemonic.starts_with("jmp")
        {
            if m != 2 && m != 6 {
                return None;
            }
            let b = code(a, m)?;
            let cc = if m == 2 { b[0] } else { b[1] } & 0xf;
            return Some(format!("k{m}:{cc:x}:{:x}", target.checked_sub(base)?));
        }
        if t.contains("rip") {
            if is_fixed(t) {
                return None;
            }
            let target = noted_target(text)?.checked_sub(base)? as i64;
            let b = code(a, m)?;
            for k in 1..m.saturating_sub(3) {
                let disp = i32::from_le_bytes(b[k..k + 4].try_into().ok()?);
                if i64::from(disp) == target - (i64::from(a) + m as i64) {
                    return Some(format!("r{m}:{k}:{target:x}"));
                }
            }
            return None;
        }
        if is_fixed(t) || t.is_empty() {
            return None;
        }
        Some(format!("c{m}"))
    };

    let mut sites: BTreeMap<u32, (usize, Vec<String>)> = BTreeMap::new();
    let mut short: Vec<&Store> = Vec::new();
    let mut rip_short = 0;
    for store in stores {
        let (rva, n, text) = &store.insn;
        let t = command(text);
        if is_masked(t) {
            continue;
        }
        if *n >= 5 {
            sites.entry(*rva).or_insert((*n, vec![format!("s{n}")]));
        } else if t.contains("rip") {
            rip_short += 1;
        } else {
            short.push(store);
        }
    }
    // 4-byte stores: the instructions after first, then those before.
    let mut left: Vec<&Store> = Vec::new();
    let mut taken: HashSet<u32> = HashSet::new();
    for store in short {
        let rva = store.insn.0;
        if taken.contains(&rva) {
            continue;
        }
        let (mut pieces, mut cur) = (vec!["s4".to_string()], rva + 4);
        for (a, m, text) in &store.after {
            if *a != cur || targets.contains(a) {
                break;
            }
            let t = command(text);
            if let Some(("jmp", target)) = branch(t) {
                if (*m == 2 || *m == 5)
                    && let Some(to) = target.checked_sub(base)
                {
                    pieces.push(format!("j{m}:{to:x}"));
                    cur += *m as u32;
                }
                break;
            }
            let Some(p) = piece(*a, *m, text) else {
                break;
            };
            pieces.push(p);
            cur += *m as u32;
            if cur - rva >= 5 {
                break;
            }
        }
        if cur - rva >= 5 {
            for k in rva + 1..cur {
                sites.remove(&k);
                taken.insert(k);
            }
            sites.insert(rva, ((cur - rva) as usize, pieces));
        } else {
            left.push(store);
        }
    }
    let free = |sites: &BTreeMap<u32, (usize, Vec<String>)>, lo: u32, hi: u32| {
        !sites
            .range(lo.saturating_sub(63)..hi)
            .any(|(&r, &(t, _))| r + t as u32 > lo)
    };
    let mut ends: HashMap<u32, u32> = sites
        .iter()
        .filter(|(_, (_, p))| !p.iter().any(|x| x.starts_with('j')))
        .map(|(&r, &(t, _))| (r + t as u32, r))
        .collect();
    let mut unpatched = 0;
    for store in left {
        let rva = store.insn.0;
        // A site that ends where this store begins takes it in.
        if !targets.contains(&rva)
            && let Some(r0) = ends.remove(&rva)
        {
            let site = sites.get_mut(&r0).expect("ends names sites");
            site.0 += 4;
            site.1.push("s4".into());
            ends.insert(rva + 4, r0);
            continue;
        }
        let (mut run, mut start) = (Vec::new(), rva);
        for (a, m, text) in &store.before {
            if a + *m as u32 != start || targets.contains(&start) {
                break;
            }
            let Some(p) = piece(*a, *m, text) else {
                break;
            };
            run.insert(0, p);
            start = *a;
            if rva + 4 - start >= 5 {
                break;
            }
        }
        if rva + 4 - start >= 5 && free(&sites, start, rva + 4) {
            run.push("s4".into());
            sites.insert(start, ((rva + 4 - start) as usize, run));
        } else {
            unpatched += 1;
        }
    }
    let (mut last, mut lines, mut in_tables) = (0u32, Vec::new(), 0);
    for (&rva, (total, pieces)) in &sites {
        if rva < last {
            continue;
        }
        if meets(tables, rva, rva + *total as u32) {
            in_tables += 1;
            continue;
        }
        lines.push(format!("{rva:x} {total} {}", pieces.join(" ")));
        last = rva + *total as u32;
    }
    Plan {
        lines,
        unpatched,
        rip_short,
        in_tables,
    }
}

pub(crate) fn run(image: &Path) -> Result<()> {
    let objdump = ["llvm-objdump", "/opt/homebrew/opt/llvm/bin/llvm-objdump"]
        .into_iter()
        .find(|o| {
            Command::new(o)
                .arg("--version")
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .context("llvm-objdump not found (brew install llvm)")?;
    let data = std::fs::read(image).with_context(|| format!("reading {}", image.display()))?;
    let secs = sections(&data)?;
    let base = secs.base;
    let text = *secs.by_name.get(".text").context("no .text section")?;
    let (tva, tsize, traw, trsize) = text;
    let code = |rva: u32, n: usize| -> Option<Vec<u8>> {
        let off = rva.checked_sub(tva)?;
        if off as usize + n > tsize.min(trsize) as usize {
            return None;
        }
        let at = (traw + off) as usize;
        data.get(at..at + n).map(<[u8]>::to_vec)
    };
    let mut targets = data_targets(&data, &secs, text);
    let functions = functions(&data, &secs);
    if functions.is_empty() {
        eprintln!("no .pdata functions in this image: nothing can be listed");
    }
    let begins: Vec<u32> = functions.iter().map(|&(b, _)| b).collect();

    let mut child = Command::new(objdump)
        .args(["-d", "--x86-asm-syntax=intel", "-j", ".text"])
        .arg(image)
        .stdout(Stdio::piped())
        .spawn()
        .context("running llvm-objdump")?;
    let reader =
        BufReader::with_capacity(1 << 20, child.stdout.take().context("llvm-objdump output")?);
    let found = scan(reader, base, (tva, tsize), &begins, &mut targets)?;
    anyhow::ensure!(child.wait()?.success(), "llvm-objdump failed");
    let all = found.stores.len();
    let kept = keep(found, &functions, (tva, tsize), &code, &mut targets);

    let plan = plan(&kept.stores, &targets, &kept.tables, &code, base);
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    writeln!(out, "{}", identity(&data).context("not a PE32+ image")?)?;
    for line in &plan.lines {
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    eprintln!(
        "{all} 256-bit stores; {} sites listed; dropped: {} outside .pdata functions, {} in \
         functions the listing is out of step with, {} in or after switch tables ({} table \
         spans), {} sites taking in table bytes; {} 4-byte stores left (no room and nothing \
         that can be moved){}",
        plan.lines.len(),
        kept.outside,
        kept.unsynced,
        kept.in_tables,
        kept.tables.len(),
        plan.in_tables,
        plan.unpatched,
        if plan.rip_short > 0 {
            format!("; {} 4-byte RIP-relative left", plan.rip_short)
        } else {
            String::new()
        }
    );
    if all == 0 {
        eprintln!("no 256-bit stores in this image");
    }
    Ok(())
}

/// The first line of the list: the image's PE TimeDateStamp, SizeOfImage
/// and CheckSum, which `victory-loader/src/wide.rs` compares with the image
/// it patches.
fn identity(data: &[u8]) -> Option<String> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
    let pe = u32_at(0x3c)? as usize;
    if data.get(pe..pe + 4)? != b"PE\0\0" || data.get(pe + 24..pe + 26)? != [0x0b, 0x02] {
        return None;
    }
    Some(format!(
        "# victory-wide-stores time={:08x} size={:08x} sum={:08x}",
        u32_at(pe + 8)?,
        u32_at(pe + 24 + 56)?,
        u32_at(pe + 24 + 64)?
    ))
}

/// An instruction without the comment `llvm-objdump` adds.
fn command(text: &str) -> &str {
    text.split(" #").next().unwrap_or(text)
}

/// `addr: bytes \t mnemonic \t operands` as `(rva, length, text)`.
fn parse(line: &str, base: u64) -> Option<Insn> {
    let (address, rest) = line.split_once(':')?;
    let address = u64::from_str_radix(address.trim(), 16).ok()?;
    let mut fields = rest.split('\t');
    let bytes = fields.next()?.split_whitespace().count();
    let mnemonic = fields.next()?.trim();
    // An instruction listed as one field with spaces (some prefixed forms) is
    // passed over, as one that cannot be moved.
    if bytes == 0 || mnemonic.is_empty() || mnemonic.contains(char::is_whitespace) {
        return None;
    }
    let operands = fields.collect::<Vec<_>>().join(" ");
    let text = format!("{mnemonic} {operands}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some((u32::try_from(address.checked_sub(base)?).ok()?, bytes, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1_4000_0000;

    /// A disassembly as `llvm-objdump -d --x86-asm-syntax=intel` prints it,
    /// from (address, bytes, mnemonic, operands); `None` a function label.
    fn listing(rows: &[Option<(u64, &[u8], &str, &str)>]) -> String {
        let mut out =
            String::from("\nimage:\tfile format coff-x86-64\n\nDisassembly of section .text:\n");
        for row in rows {
            match row {
                Some((at, bytes, mnemonic, operands)) => {
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    out += &format!("{at:x}: {:<30}\t{mnemonic}\t{operands}\n", hex.join(" "));
                }
                None => out += "\n0000000140001000 <f>:\n",
            }
        }
        out
    }

    /// What the listing of `rows` comes to.
    struct Listed {
        lines: Vec<String>,
        unpatched: usize,
        rip_short: usize,
        kept: Kept,
        in_table_sites: usize,
    }

    /// [`scan`], [`keep`] and [`plan`] over the listing of `rows`, `.text`
    /// being 0x1000..0x201000 (its bytes those of `rows` and `raw`, 0xcc
    /// elsewhere), with `extra` branch targets and these `.pdata` functions.
    fn list(
        rows: &[Option<(u64, &[u8], &str, &str)>],
        raw: &[(u32, &[u8])],
        functions: &[(u32, u32)],
        extra: &[u32],
    ) -> Listed {
        let text = (0x1000u32, 0x20_0000u32);
        let mut bytes = vec![0xccu8; text.1 as usize];
        let rows_bytes = rows
            .iter()
            .flatten()
            .map(|(at, b, _, _)| ((at - BASE) as u32, *b));
        for (rva, b) in rows_bytes.chain(raw.iter().copied()) {
            let at = (rva - text.0) as usize;
            bytes[at..at + b.len()].copy_from_slice(b);
        }
        let code = |rva: u32, n: usize| {
            let at = rva.checked_sub(text.0)? as usize;
            bytes.get(at..at + n).map(<[u8]>::to_vec)
        };
        let mut targets: HashSet<u32> = extra.iter().copied().collect();
        targets.extend(functions.iter().flat_map(|&(b, e)| [b, e]));
        let begins: Vec<u32> = functions.iter().map(|&(b, _)| b).collect();
        let found = scan(listing(rows).as_bytes(), BASE, text, &begins, &mut targets).unwrap();
        let kept = keep(found, functions, text, &code, &mut targets);
        let p = plan(&kept.stores, &targets, &kept.tables, &code, BASE);
        Listed {
            lines: p.lines,
            unpatched: p.unpatched,
            rip_short: p.rip_short,
            kept,
            in_table_sites: p.in_tables,
        }
    }

    /// The lines [`plan`] makes of `rows`, one function holding them all,
    /// with `extra` branch targets.
    fn plan_of(
        rows: &[Option<(u64, &[u8], &str, &str)>],
        extra: &[u32],
    ) -> (Vec<String>, usize, usize) {
        let all = rows.iter().flatten();
        let begin = all.clone().map(|r| (r.0 - BASE) as u32).min().unwrap();
        let end = all
            .map(|r| (r.0 - BASE) as u32 + r.1.len() as u32)
            .max()
            .unwrap();
        let l = list(rows, &[], &[(begin, end)], extra);
        (l.lines, l.unpatched, l.rip_short)
    }

    const STORE4: &[u8] = &[0xc5, 0xfc, 0x11, 0x07];
    const MOV5: &[u8] = &[0xb8, 1, 0, 0, 0];

    #[test]
    fn reads_objdump_lines() {
        let line = "   140001000: c5 fc 11 07                  \tvmovups\tymmword ptr [rdi], ymm0";
        assert_eq!(
            parse(line, BASE),
            Some((0x1000, 4, "vmovups ymmword ptr [rdi], ymm0".to_string()))
        );
        let line = "140001004: 48 8d 05 50 00 00 00 \tlea\trax, [rip + 0x50]  \t# 0x14000105b";
        let (_, n, text) = parse(line, BASE).unwrap();
        assert_eq!((n, command(&text)), (7, "lea rax, [rip + 0x50]"));
        assert_eq!(noted_target(&text), Some(0x1_4000_105b));
        // Labels, headers, addresses below the image, prefixed forms in one field.
        for other in [
            "0000000140001000 <f>:",
            "Disassembly of section .text:",
            "1000: c3 \tret",
            "140001000: f3 48 ab \trep stosq\t",
        ] {
            assert_eq!(parse(other, BASE), None, "{other:?}");
        }
    }

    #[test]
    fn knows_wide_stores_branches_and_what_cannot_move() {
        assert!(is_store("vmovups ymmword ptr [r8 + 0x34], ymm0"));
        assert!(is_store("vmovdqu ymmword ptr [rax + 8*rcx + 0x40], ymm9"));
        assert!(!is_store("vmovups xmmword ptr [rdi], xmm0"));
        assert!(!is_store("vmovups ymm0, ymmword ptr [rdi]"));
        assert!(!is_store("vmovups ymmword ptr [rdi], ymm"));
        assert!(is_masked("vmaskmovps ymmword ptr [rdi], ymm1, ymm0"));
        assert_eq!(
            branch("jne 0x140001020 <f+0x20>"),
            Some(("jne", 0x1_4000_1020))
        );
        assert_eq!(branch("call 0x140002000"), Some(("call", 0x1_4000_2000)));
        assert_eq!(branch("jmp rax"), None);
        assert_eq!(branch("mov eax, 0x1"), None);
        for fixed in [
            "call rax",
            "ret",
            "push rbx",
            "jmp rax",
            "int3",
            "int 0x29",
            "lock xadd dword ptr [rcx], eax",
        ] {
            assert!(is_fixed(fixed), "{fixed:?}");
        }
        for movable in [
            "mov eax, 0x1",
            "interrupt_like_name eax",
            "xor eax, eax",
            "lea rax, [rdi + 0x8]",
        ] {
            assert!(!is_fixed(movable), "{movable:?}");
        }
    }

    #[test]
    fn a_short_store_takes_in_what_follows_it() {
        let rows = [
            None,
            Some((0x1_4000_1000, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((0x1_4000_1004, MOV5, "mov", "eax, 0x1")),
            Some((
                0x1_4000_1009,
                &[0xc5, 0xfc, 0x11, 0x47, 0x20][..],
                "vmovups",
                "ymmword ptr [rdi + 0x20], ymm0",
            )),
            Some((0x1_4000_100e, &[0xc3][..], "ret", "")),
        ];
        assert_eq!(
            plan_of(&rows, &[]),
            (vec!["1000 9 s4 c5".into(), "1009 5 s5".into()], 0, 0)
        );
    }

    #[test]
    fn branches_and_rip_relative_neighbours_are_listed_for_redoing() {
        let rows = [
            None,
            Some((0x1_4000_1000, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((
                0x1_4000_1004,
                &[0x75, 0x1a][..],
                "jne",
                "0x140001020 <f+0x20>",
            )),
            Some((0x1_4000_1006, STORE4, "vmovups", "ymmword ptr [rsi], ymm1")),
            Some((
                0x1_4000_100a,
                &[0x48, 0x8d, 0x05, 0x4a, 0, 0, 0][..],
                "lea",
                "rax, [rip + 0x4a] # 0x14000105b",
            )),
            Some((0x1_4000_1011, &[0xc3][..], "ret", "")),
        ];
        let (lines, unpatched, _) = plan_of(&rows, &[]);
        assert_eq!(lines, ["1000 6 s4 k2:5:1020", "1006 11 s4 r7:3:105b"]);
        assert_eq!(unpatched, 0);
    }

    #[test]
    fn a_short_store_with_a_fixed_follower_takes_in_what_precedes_it() {
        let rows = [
            None,
            Some((0x1_4000_2000, &[0x48, 0x89, 0xc8][..], "mov", "rax, rcx")),
            Some((0x1_4000_2003, &[0x31, 0xc0][..], "xor", "eax, eax")),
            Some((0x1_4000_2005, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((0x1_4000_2009, &[0xc3][..], "ret", "")),
        ];
        assert_eq!(plan_of(&rows, &[]).0, ["2003 6 c2 s4"]);
    }

    #[test]
    fn never_across_a_branch_target_or_a_function_bound() {
        let rows = [
            None,
            Some((0x1_4000_2000, &[0x48, 0x89, 0xc8][..], "mov", "rax, rcx")),
            Some((0x1_4000_2003, &[0x31, 0xc0][..], "xor", "eax, eax")),
            Some((0x1_4000_2005, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((0x1_4000_2009, MOV5, "mov", "eax, 0x1")),
            Some((0x1_4000_200e, &[0xc3][..], "ret", "")),
        ];
        // Something jumps to the mov after the store and to the store itself
        // (or a .pdata entry begins there): the mov may not join the store,
        // nor may the store join what precedes it.
        assert_eq!(plan_of(&rows, &[0x2009, 0x2005]), (vec![], 1, 0));
        // A target at the mov only: the run goes backwards instead.
        assert_eq!(plan_of(&rows, &[0x2009]).0, ["2003 6 c2 s4"]);
        // A direct branch elsewhere in the listing counts as one too.
        let mut with_jump = rows.to_vec();
        with_jump.push(Some((
            0x1_4000_200f,
            &[0xeb, 0xf8][..],
            "jmp",
            "0x140002009 <f+0x9>",
        )));
        assert_eq!(plan_of(&with_jump, &[0x2005]), (vec![], 1, 0));
    }

    #[test]
    fn masked_and_rip_relative_short_stores_are_left() {
        let rows = [
            None,
            Some((
                0x1_4000_3000,
                &[0xc4, 0xe2, 0x75, 0x2e, 0x07][..],
                "vmaskmovps",
                "ymmword ptr [rdi], ymm1, ymm0",
            )),
            Some((
                0x1_4000_3005,
                &[0xc5, 0xfc, 0x11, 0x05][..],
                "vmovups",
                "ymmword ptr [rip], ymm0",
            )),
            Some((0x1_4000_3009, &[0xc3][..], "ret", "")),
        ];
        assert_eq!(plan_of(&rows, &[]), (vec![], 0, 1));
    }

    #[test]
    fn reads_the_sections_of_a_pe32_plus_image() {
        let mut pe = vec![0u8; 0x400];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x86..0x88].copy_from_slice(&3u16.to_le_bytes()); // sections
        pe[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes()); // optional header size
        pe[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        pe[0x98 + 24..0x98 + 32].copy_from_slice(&BASE.to_le_bytes());
        let table = 0x98 + 0xf0;
        for (i, (name, va, vsize, raw, rsize)) in [
            (".text", 0x1000u32, 0x20u32, 0x200u32, 0x100u32),
            (".rdata", 0x2000, 0x8, 0x300, 0x80),
            (".reloc", 0x3000, 0x10, 0x380, 0x10),
        ]
        .into_iter()
        .enumerate()
        {
            let at = table + 40 * i;
            pe[at..at + name.len()].copy_from_slice(name.as_bytes());
            for (k, v) in [(8, vsize), (12, va), (16, rsize), (20, raw)] {
                pe[at + k..at + k + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        // .rdata holds one word that is an address in .text and one that is not.
        pe[0x300..0x304].copy_from_slice(&0x1010u32.to_le_bytes());
        pe[0x304..0x308].copy_from_slice(&0x5000u32.to_le_bytes());
        // A 64-bit address of the code at .rdata+8, which .reloc names
        // (DIR64 at 0x2008), and a 32-bit fixup (HIGHLOW) at 0x2000, passed over.
        pe[0x308..0x310].copy_from_slice(&(BASE + 0x1018).to_le_bytes());
        pe[0x380..0x384].copy_from_slice(&0x2000u32.to_le_bytes());
        pe[0x384..0x388].copy_from_slice(&0x0cu32.to_le_bytes());
        pe[0x388..0x38a].copy_from_slice(&0xa008u16.to_le_bytes());
        pe[0x38a..0x38c].copy_from_slice(&0x3000u16.to_le_bytes());
        let secs = sections(&pe).unwrap();
        assert_eq!(secs.base, BASE);
        let text = secs.by_name[".text"];
        assert_eq!(text, (0x1000, 0x20, 0x200, 0x100));
        assert_eq!(
            data_targets(&pe, &secs, text),
            HashSet::from([0x1010, 0x1018])
        );
        assert_eq!(at_rva(&pe, &secs, 0x2008, 8), Some(&pe[0x308..0x310]));
        assert_eq!(at_rva(&pe, &secs, 0x4000, 4), None);
        // Not a PE, cut short, 32-bit.
        assert!(
            sections(b"not a PE image at all, long enough to hold a header, surely..").is_err()
        );
        assert!(sections(&pe[..0x90]).is_err());
        let mut pe32 = pe.clone();
        pe32[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
        assert!(sections(&pe32).is_err());
    }

    /// The switch of an MSVC x64 function: `lea rdx, [image base]`, the
    /// table's entry, `jmp`; its table of RVAs after the code, in `.text`.
    fn switch_rows() -> Vec<Option<(u64, &'static [u8], &'static str, &'static str)>> {
        vec![
            None,
            Some((
                0x1_4000_1000,
                &[0x48, 0x8d, 0x15, 0xf9, 0xef, 0xff, 0xff][..],
                "lea",
                "rdx, [rip - 0x1007] # 0x140000000",
            )),
            Some((
                0x1_4000_1007,
                &[0x8b, 0x8c, 0x82, 0x00, 0x11, 0, 0][..],
                "mov",
                "ecx, dword ptr [rdx + 4*rax + 0x1100]",
            )),
            Some((0x1_4000_100e, &[0x48, 0x01, 0xd1][..], "add", "rcx, rdx")),
            Some((0x1_4000_1011, &[0xff, 0xe1][..], "jmp", "rcx")),
            Some((
                0x1_4000_1013,
                &[0xc5, 0xfc, 0x11, 0x47, 0x20][..],
                "vmovups",
                "ymmword ptr [rdi + 0x20], ymm0",
            )),
            Some((0x1_4000_1018, &[0xc3][..], "ret", "")),
            // The table as the listing decodes it: its second entry,
            // 0x0011fcc5, reads as a store.
            Some((
                0x1_4000_1100,
                &[0x20, 0x10, 0, 0][..],
                "and",
                "byte ptr [rax], dl",
            )),
            Some((
                0x1_4000_1104,
                &[0xc5, 0xfc, 0x11, 0x00][..],
                "vmovups",
                "ymmword ptr [rax], ymm0",
            )),
            Some((0x1_4000_1108, &[0xc3][..], "ret", "")),
        ]
    }

    #[test]
    fn a_switch_table_in_the_code_is_never_listed() {
        let l = list(&switch_rows(), &[], &[(0x1000, 0x1110)], &[]);
        assert_eq!(l.lines, ["1013 5 s5"]);
        assert_eq!(l.kept.tables, [(0x1100, 0x1108)]);
        assert_eq!(
            (l.kept.in_tables, l.kept.outside, l.kept.unsynced),
            (1, 0, 0)
        );
        // Without the load that names it, the table's entry would be listed.
        let mut rows = switch_rows();
        rows[2] = Some((
            0x1_4000_1007,
            &[0x8b, 0x0c, 0x82, 0x90, 0x90, 0x90, 0x90][..],
            "mov",
            "ecx, dword ptr [rdx + 4*rax]",
        ));
        assert_eq!(
            list(&rows, &[], &[(0x1000, 0x1110)], &[]).lines,
            ["1013 5 s5", "1100 8 c4 s4"]
        );
    }

    #[test]
    fn a_table_entry_and_a_lea_into_the_code_are_branch_targets() {
        let rows = [
            None,
            Some((
                0x1_4000_1000,
                &[0x48, 0x8d, 0x05, 0x04, 0, 0, 0][..],
                "lea",
                "rax, [rip + 0x4] # 0x14000100b",
            )),
            Some((0x1_4000_1007, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((0x1_4000_100b, MOV5, "mov", "eax, 0x1")),
            Some((
                0x1_4000_1010,
                &[0x8b, 0x8c, 0x82, 0x00, 0x11, 0, 0][..],
                "mov",
                "ecx, dword ptr [rdx + 4*rax + 0x1100]",
            )),
            Some((0x1_4000_1017, &[0xc3][..], "ret", "")),
            Some((
                0x1_4000_1018,
                &[0x0f, 0x1f, 0x40, 0x00][..],
                "nop",
                "dword ptr [rax]",
            )),
            Some((0x1_4000_101c, STORE4, "vmovups", "ymmword ptr [rsi], ymm0")),
            Some((0x1_4000_1020, MOV5, "mov", "eax, 0x1")),
            Some((0x1_4000_1025, &[0xc3][..], "ret", "")),
        ];
        // The table's one entry is 0x1020.
        let raw: [(u32, &[u8]); 1] = [(0x1100, &[0x20, 0x10, 0, 0])];
        let l = list(&rows, &raw, &[(0x1000, 0x1030)], &[]);
        // Neither store may take in the mov after it, which the lea (0x100b)
        // or the table (0x1020) may jump to: each takes in what precedes it.
        assert_eq!(l.lines, ["1000 11 r7:3:100b s4", "1018 8 c4 s4"]);
        assert_eq!(
            (l.unpatched, l.kept.tables.as_slice()),
            (0, &[(0x1100, 0x1104)][..])
        );
        let mut targets = HashSet::new();
        let found = scan(
            listing(&rows).as_bytes(),
            BASE,
            (0x1000, 0x20_0000),
            &[0x1000],
            &mut targets,
        )
        .unwrap();
        assert!(targets.contains(&0x100b), "the lea's target");
        assert_eq!(found.lea_text, [0x100b]);
        assert_eq!(found.dword_tables, [(0x1010, 0x1100)]);
    }

    #[test]
    fn only_stores_inside_functions_the_listing_is_in_step_with_are_kept() {
        let rows = [
            None,
            Some((0x1_4000_1000, STORE4, "vmovups", "ymmword ptr [rdi], ymm0")),
            Some((0x1_4000_1004, MOV5, "mov", "eax, 0x1")),
            Some((0x1_4000_1009, &[0xc3][..], "ret", "")),
            // A leaf without unwind data: no .pdata entry.
            Some((
                0x1_4000_1010,
                &[0xc5, 0xfc, 0x11, 0x47, 0x20][..],
                "vmovups",
                "ymmword ptr [rdi + 0x20], ymm0",
            )),
            Some((0x1_4000_1015, &[0xc3][..], "ret", "")),
            // A function whose begin, 0x1021, falls inside an instruction.
            Some((0x1_4000_1020, &[0x48, 0x89, 0xc8][..], "mov", "rax, rcx")),
            Some((
                0x1_4000_1023,
                &[0xc5, 0xfc, 0x11, 0x47, 0x20][..],
                "vmovups",
                "ymmword ptr [rdi + 0x20], ymm0",
            )),
            Some((0x1_4000_1028, &[0xc3][..], "ret", "")),
            // A store across its function's end.
            Some((
                0x1_4000_1030,
                &[0xc5, 0xfc, 0x11, 0x47, 0x20][..],
                "vmovups",
                "ymmword ptr [rdi + 0x20], ymm0",
            )),
        ];
        let l = list(
            &rows,
            &[],
            &[(0x1000, 0x100a), (0x1021, 0x1029), (0x1030, 0x1033)],
            &[],
        );
        assert_eq!(l.lines, ["1000 9 s4 c5"]);
        assert_eq!(
            (l.kept.outside, l.kept.unsynced, l.kept.in_tables),
            (2, 1, 0)
        );
        assert_eq!(l.in_table_sites, 0);
    }

    #[test]
    fn the_list_names_the_image_it_was_made_from() {
        let mut pe = vec![0u8; 0x200];
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        pe[0x88..0x8c].copy_from_slice(&0x6512_34abu32.to_le_bytes());
        pe[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        pe[0x98 + 56..0x98 + 60].copy_from_slice(&0x0a00_0000u32.to_le_bytes());
        pe[0x98 + 64..0x98 + 68].copy_from_slice(&0x0bad_f00du32.to_le_bytes());
        // The same line `victory-loader/src/wide.rs` expects.
        assert_eq!(
            identity(&pe).as_deref(),
            Some("# victory-wide-stores time=651234ab size=0a000000 sum=0badf00d")
        );
        pe[0x98] = 0x0b;
        pe[0x99] = 0x01;
        assert_eq!(identity(&pe), None);
    }
}
