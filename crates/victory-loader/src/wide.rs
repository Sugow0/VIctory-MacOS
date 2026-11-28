//! 256-bit vector stores redone as two 128-bit stores, for Rosetta 2
//! (`VICTORY_PATCH_WIDE=<file>`, `VICTORY_PATCH_WIDE_DIR=<folder>`; off
//! without them).
//!
//! Ported from pragyaangaur/VIctory-macOS (GPL-3.0-or-later, the same
//! upstream as ours): `crates/victory-loader/src/wide.rs` of its PR #262,
//! with PR #556 (TLS-redirect calls copied into a trampoline re-aimed).
//! Changed here: the image's size bounds every site, a site whose bytes
//! another of our patches already rewrote is refused (`patched.rs`), a page
//! that cannot be made writable refuses its site instead of being written,
//! and the refusals are counted by reason in the log.
//!
//! Rosetta 2 cannot take a fault in the second page of a 32-byte store that
//! began in a writable page: it aborts with "unexpectedly need to
//! EmulateForward on a synchronous exception" before any handler runs.
//! `vmovdqu ymm`, `vmovups ymm` and `vmaskmov ymm` die; 128-bit stores,
//! `vextractf128` to memory and smaller stores fault and resume normally.
//! Under Wine, write watching (`MEM_WRITE_WATCH`) and our view guards
//! (`victory-gpu/src/watch.rs`) write-protect title pages, so such a store
//! across a page boundary (`vmovups [r8+0x34], ymm0`) stops the title.
//!
//! `VICTORY_PATCH_WIDE=<file>` names a list of sites found offline with a
//! disassembler (`cargo xtask wide-stores`), one line each: RVA in hex, bytes
//! replaced, then pieces: `sN` a 256-bit store of N bytes, split; `cN` N bytes
//! copied as they are; `rN:K:T` N bytes copied whose RIP-relative
//! displacement at K is redone for RVA T; `kN:CC:T` a conditional jump to T;
//! `jN:T` a jump of N bytes to RVA T, which ends the run. Each site becomes a
//! jump to a trampoline that stores the low half with the same instruction at
//! 128 bits and the high half with `vextractf128 [m+16], ymm, 1`. Every
//! piece is checked against the image's bytes before it is patched; a site
//! that does not match is left alone and counted.

/// A store piece decoded: VEX fields and its memory operand.
#[derive(Debug, Clone, Copy)]
struct Wide {
    rxb: u8, // inverted R, X, B bits as in the VEX prefix (bits 2..0)
    map: u8,
    w: bool,
    vvvv: u8,
    pp: u8,
    opcode: u8,
    modrm: u8,
    sib: Option<u8>,
    disp: i32,
}

/// `b` as one 256-bit VEX store to memory (`vmovups`, `vmovupd`, `vmovaps`,
/// `vmovapd`, `vmovdqu`, `vmovdqa`, `vmovntps`, `vmovntpd`, `vmovntdq`),
/// all of `b` and nothing more.
fn decode(b: &[u8]) -> Option<Wide> {
    let (rxb, map, w, vvvv, l, pp, rest) = match *b.first()? {
        0xc5 => {
            let p = *b.get(1)?;
            (
                (p >> 7) << 2 | 0b011,
                1,
                false,
                (p >> 3) & 15,
                (p >> 2) & 1,
                p & 3,
                2,
            )
        }
        0xc4 => {
            let (p1, p2) = (*b.get(1)?, *b.get(2)?);
            (
                p1 >> 5,
                p1 & 31,
                p2 & 0x80 != 0,
                (p2 >> 3) & 15,
                (p2 >> 2) & 1,
                p2 & 3,
                3,
            )
        }
        _ => return None,
    };
    let opcode = *b.get(rest)?;
    let modrm = *b.get(rest + 1)?;
    // The prefix that goes with each store at 256 bits: `F3`/`F2 11` are
    // `vmovss`/`vmovsd`, whose L bit is ignored, and stores have no vvvv.
    let store = match opcode {
        0x11 | 0x29 | 0x2b => matches!(pp, 0 | 1),
        0x7f => matches!(pp, 1 | 2),
        0xe7 => pp == 1,
        _ => false,
    };
    if map != 1 || l != 1 || !store || vvvv != 15 || modrm >> 6 == 3 {
        return None;
    }
    let (md, rm) = (modrm >> 6, modrm & 7);
    let mut at = rest + 2;
    let sib = if rm == 4 {
        at += 1;
        Some(*b.get(at - 1)?)
    } else {
        None
    };
    let disp_len = match md {
        1 => 1,
        2 => 4,
        _ if rm == 5 || sib.is_some_and(|s| s & 7 == 5) => 4,
        _ => 0,
    };
    let d = b.get(at..at + disp_len)?;
    let disp = match disp_len {
        1 => i32::from(d[0] as i8),
        4 => i32::from_le_bytes(d.try_into().ok()?),
        _ => 0,
    };
    if at + disp_len != b.len() {
        return None;
    }
    Some(Wide {
        rxb,
        map,
        w,
        vvvv,
        pp,
        opcode,
        modrm,
        sib,
        disp,
    })
}

/// The memory operand of `s` with `extra` added to its displacement, always
/// with a 32-bit displacement, appended to `c` (whose first byte lies at
/// `at`). `rip`: the original instruction's end, for a RIP-relative operand;
/// `end_after`: the bytes of the new instruction after its displacement.
fn memory(
    c: &mut Vec<u8>,
    s: &Wide,
    extra: i32,
    rip: u64,
    end_after: usize,
    at: u64,
) -> Option<()> {
    let (md, rm, reg) = (s.modrm >> 6, s.modrm & 7, s.modrm & 0x38);
    if md == 0 && rm == 5 {
        c.push(reg | 5);
        let target = rip as i64 + i64::from(s.disp) + i64::from(extra);
        let end = at as i64 + c.len() as i64 + 4 + end_after as i64;
        c.extend_from_slice(&i32::try_from(target - end).ok()?.to_le_bytes());
    } else if md == 0 && s.sib.is_some_and(|x| x & 7 == 5) {
        c.push(reg | 4);
        c.push(s.sib?);
        c.extend_from_slice(&s.disp.checked_add(extra)?.to_le_bytes());
    } else {
        c.push(0x80 | reg | rm);
        if let Some(x) = s.sib {
            c.push(x);
        }
        c.extend_from_slice(&s.disp.checked_add(extra)?.to_le_bytes());
    }
    Some(())
}

/// The two 128-bit stores for `s`, appended to `c` (whose first byte will
/// lie at `base_at`).
fn split(c: &mut Vec<u8>, s: &Wide, rip: u64, base_at: u64) -> Option<()> {
    // Low half: the same store at 128 bits.
    c.extend_from_slice(&[
        0xc4,
        s.rxb << 5 | s.map,
        u8::from(s.w) << 7 | s.vvvv << 3 | s.pp,
        s.opcode,
    ]);
    memory(c, s, 0, rip, 0, base_at)?;
    // High half: vextractf128 [m+16], ymm, 1.
    c.extend_from_slice(&[0xc4, s.rxb << 5 | 3, 0x7d, 0x19]);
    memory(c, s, 16, rip, 1, base_at)?;
    c.push(1);
    Some(())
}

#[derive(Debug, Clone)]
enum Piece {
    Store(usize),
    Copy(usize),
    Jump(usize, u32),
    /// A conditional jump of N bytes, its condition, its target RVA.
    Cond(usize, u8, u32),
    /// N bytes copied whose 32-bit displacement at K is RIP-relative to RVA T.
    Rip(usize, usize, u32),
}

/// Where a 4-byte site jumps first: padding slots (RVAs) jumped through
/// with 2-byte jumps, the last of 5 bytes holding the jump to the trampoline.
type Route = Vec<u32>;

/// One line of a site list: RVA, bytes replaced, pieces, route.
fn parse(line: &str) -> Option<(u32, usize, Vec<Piece>, Route)> {
    let mut f = line.split_whitespace();
    let rva = u32::from_str_radix(f.next()?, 16).ok()?;
    let total: usize = f.next()?.parse().ok()?;
    let mut pieces = Vec::new();
    let mut route = Route::new();
    let hex = |t: &str| u32::from_str_radix(t, 16).ok();
    for p in f {
        if !p.is_char_boundary(1) {
            return None;
        }
        let (k, v) = p.split_at(1);
        let parts: Vec<&str> = v.split(':').collect();
        pieces.push(match (k, parts.as_slice()) {
            ("s", [n]) => Piece::Store(n.parse().ok()?),
            ("c", [n]) => Piece::Copy(n.parse().ok()?),
            ("j", [n, t]) => Piece::Jump(n.parse().ok()?, hex(t)?),
            ("k", [n, cc, t]) => {
                Piece::Cond(n.parse().ok()?, u8::from_str_radix(cc, 16).ok()?, hex(t)?)
            }
            ("r", [n, at, t]) => Piece::Rip(n.parse().ok()?, at.parse().ok()?, hex(t)?),
            ("@", _) => {
                route = v.split(',').map(hex).collect::<Option<_>>()?;
                continue;
            }
            _ => return None,
        });
    }
    let sum: Option<usize> = pieces
        .iter()
        .map(|p| match p {
            Piece::Store(n)
            | Piece::Copy(n)
            | Piece::Jump(n, _)
            | Piece::Cond(n, ..)
            | Piece::Rip(n, ..) => *n,
        })
        .try_fold(0usize, |acc, n| acc.checked_add(n));
    let sum = sum?;
    (sum == total && (total >= 5 || !route.is_empty() && total >= 2))
        .then_some((rva, total, pieces, route))
}

/// The first line of a site list: which image it was made from, as
/// `cargo xtask wide-stores` writes it (the PE header's TimeDateStamp,
/// SizeOfImage and CheckSum). `None` if `headers` are not PE32+ headers.
fn identity(headers: &[u8]) -> Option<String> {
    let u32_at = |at: usize| {
        Some(u32::from_le_bytes(
            headers.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    if headers.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3c)? as usize;
    if headers.get(pe..pe + 4)? != b"PE\0\0" || headers.get(pe + 24..pe + 26)? != [0x0b, 0x02] {
        return None;
    }
    Some(format!(
        "# victory-wide-stores time={:08x} size={:08x} sum={:08x}",
        u32_at(pe + 8)?,
        u32_at(pe + 24 + 56)?,
        u32_at(pe + 24 + 64)?
    ))
}

/// Whether the list `text` was made from the image whose headers are
/// `headers`: its first line names it. A list without that line, or made
/// from another build, is refused whole.
fn made_from(text: &str, headers: &[u8]) -> bool {
    let first = text.lines().next().map(str::trim);
    identity(headers).is_some_and(|id| first == Some(id.as_str()))
}

/// What `tls.rs` writes over a 9-byte `mov r64, gs:[0x58]`: `call rel32`
/// then a 4-byte NOP.
const TLS_CALL_LEN: usize = 9;
const TLS_CALL_FILL: [u8; 4] = [0x0f, 0x1f, 0x40, 0x00];

/// Where one image's trampolines lie, so that a fault or a return address
/// in one can be named by the site it stands for.
struct Pool {
    start: u64,
    size: u64,
    used: u64,
    image: u64,
    /// (offset in the pool, site RVA), by offset.
    sites: Vec<(u32, u32)>,
}

/// What an address in a trampoline pool stands for.
#[derive(Debug, PartialEq, Eq)]
pub enum InPool {
    /// `into` bytes into the trampoline of the site at `image + rva`.
    Site { image: u64, rva: u32, into: u64 },
    /// Reserved for trampolines but holding none: nothing should run there.
    Unused { image: u64 },
}

impl Pool {
    fn find(&self, address: u64) -> Option<InPool> {
        let off = address.checked_sub(self.start)?;
        if off >= self.size {
            return None;
        }
        let image = self.image;
        let i = self.sites.partition_point(|&(o, _)| u64::from(o) <= off);
        match i.checked_sub(1).map(|i| self.sites[i]) {
            Some((o, rva)) if off < self.used => Some(InPool::Site {
                image,
                rva,
                into: off - u64::from(o),
            }),
            _ => Some(InPool::Unused { image }),
        }
    }
}

static POOLS: std::sync::Mutex<Vec<Pool>> = std::sync::Mutex::new(Vec::new());

/// The wide store trampoline `address` lies in, if any. For crash reports:
/// gives up rather than wait if the pools are being written.
pub fn find(address: u64) -> Option<InPool> {
    let pools = POOLS.try_lock().ok()?;
    pools.iter().find_map(|p| p.find(address))
}

/// The trampoline for one site, its first byte at `at`; `code` the image's
/// bytes at the site. `None` if a piece does not match its bytes.
fn trampoline(
    base: u64,
    rva: u32,
    total: usize,
    pieces: &[Piece],
    code: &[u8],
    at: u64,
) -> Option<Vec<u8>> {
    let site = base + u64::from(rva);
    let mut c = Vec::with_capacity(64);
    let mut off = 0usize;
    for p in pieces {
        match *p {
            Piece::Store(n) => {
                let s = decode(code.get(off..off + n)?)?;
                let rip = site + (off + n) as u64;
                split(&mut c, &s, rip, at)?;
                off += n;
            }
            Piece::Copy(n) => {
                let b = code.get(off..off + n)?;
                // Nothing that moves with its address may be copied.
                if b.is_empty() || b.iter().all(|&x| x == 0xcc) {
                    return None;
                }
                // The list names one instruction per copy, never a branch
                // (`cargo xtask wide-stores` keeps calls and jumps out), but
                // `code` is the image as mapped: the static TLS redirect
                // (`tls.rs`) has by now turned `mov r64, gs:[0x58]` into
                // `call thunk; nop4`. Copied as it is, the call would land
                // as far from its thunk as the trampoline is from the site
                // (pragyaangaur/VIctory-macOS #556: a call into nothing).
                match b[0] {
                    0xe8 if n == TLS_CALL_LEN && b[5..] == TLS_CALL_FILL => {
                        let old = i32::from_le_bytes(b[1..5].try_into().ok()?);
                        let to = (site + (off + 5) as u64) as i64 + i64::from(old);
                        let next = at as i64 + c.len() as i64 + 5;
                        c.push(0xe8);
                        c.extend_from_slice(&i32::try_from(to - next).ok()?.to_le_bytes());
                        c.extend_from_slice(&b[5..]);
                    }
                    // Another branch written over the listed instruction
                    // (`write_sites.rs`, `descriptor_sites.rs` hooks): what it
                    // reaches and what follows it are not known here.
                    0xe8 | 0xe9 | 0xeb => return None,
                    _ => c.extend_from_slice(b),
                }
                off += n;
            }
            Piece::Cond(n, cc, target) => {
                let b = code.get(off..off + n)?;
                let ok = n == 2 && b[0] == 0x70 | cc || n == 6 && b[0] == 0x0f && b[1] == 0x80 | cc;
                if !ok || cc > 15 {
                    return None;
                }
                // The displacement must be the one the list was made with.
                let rel = if n == 2 {
                    i64::from(b[1] as i8)
                } else {
                    i64::from(i32::from_le_bytes(b[2..6].try_into().ok()?))
                };
                let to = base + u64::from(target);
                if (site + (off + n) as u64) as i64 + rel != to as i64 {
                    return None;
                }
                let next = at as i64 + c.len() as i64 + 6;
                c.extend_from_slice(&[0x0f, 0x80 | cc]);
                c.extend_from_slice(&i32::try_from(to as i64 - next).ok()?.to_le_bytes());
                off += n;
            }
            Piece::Rip(n, k, target) => {
                let b = code.get(off..off + n)?;
                let old = i32::from_le_bytes(b.get(k..k + 4)?.try_into().ok()?);
                // The displacement must be the one the list was made with.
                if (site + (off + n) as u64) as i64 + i64::from(old)
                    != (base + u64::from(target)) as i64
                {
                    return None;
                }
                let end = at as i64 + (c.len() + n) as i64;
                let new = i32::try_from((base + u64::from(target)) as i64 - end).ok()?;
                let from = c.len();
                c.extend_from_slice(b);
                c[from + k..from + k + 4].copy_from_slice(&new.to_le_bytes());
                off += n;
            }
            Piece::Jump(n, target) => {
                let b = code.get(off..off + n)?;
                let rel = match (b[0], n) {
                    (0xe9, 5) => i64::from(i32::from_le_bytes(b[1..5].try_into().ok()?)),
                    (0xeb, 2) => i64::from(b[1] as i8),
                    _ => return None,
                };
                let to = base + u64::from(target);
                if (site + (off + n) as u64) as i64 + rel != to as i64 {
                    return None;
                }
                let next = at as i64 + c.len() as i64 + 5;
                c.push(0xe9);
                c.extend_from_slice(&i32::try_from(to as i64 - next).ok()?.to_le_bytes());
                return (off + n == total).then_some(c);
            }
        }
    }
    if off != total {
        return None;
    }
    let back = site + total as u64;
    let next = at as i64 + c.len() as i64 + 5;
    c.push(0xe9);
    c.extend_from_slice(&i32::try_from(back as i64 - next).ok()?.to_le_bytes());
    Some(c)
}

/// Why a listed site was left as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Refused {
    /// Its bytes are not the ones the list was made from.
    Bytes,
    /// It lies, or its route does, outside the image.
    Outside,
    /// Another of our patches rewrote some of its bytes already.
    Overlap,
    /// The trampoline or a hop is out of a jump's reach.
    Reach,
    /// A page of it could not be made writable.
    Protect,
    /// The pool is full.
    Full,
}

/// The pool's size: 117,413 sites of the 1.0.2.1 executable take about 5 MiB.
const POOL_SIZE: usize = 16 << 20;

/// [`patch`] with the list `VICTORY_PATCH_WIDE` names, for the executable
/// mapped at `base` with `size` bytes: `None` without the switch (unset,
/// empty or `0`).
///
/// # Safety
/// As [`patch`].
#[cfg(windows)]
pub unsafe fn patch_asked(base: u64, size: usize) -> Option<(usize, usize)> {
    let list = std::env::var_os("VICTORY_PATCH_WIDE").filter(|v| !v.is_empty() && v != "0")?;
    // SAFETY: by the caller.
    Some(unsafe { patch(base, size, std::path::Path::new(&list)) })
}

/// [`patch`] for a title DLL, with `<VICTORY_PATCH_WIDE_DIR>/<hash16>.txt`
/// (`hash16`: the 16 hex digits that name its cached image,
/// `victory-image-<hash16>.dll`) if the switch is set and the file is there.
///
/// # Safety
/// As [`patch`].
#[cfg(windows)]
pub unsafe fn patch_dll_asked(base: u64, size: usize, hash16: &str) -> Option<(usize, usize)> {
    let dir = std::env::var_os("VICTORY_PATCH_WIDE_DIR").filter(|v| !v.is_empty())?;
    let list = std::path::Path::new(&dir).join(format!("{hash16}.txt"));
    // SAFETY: by the caller.
    list.is_file().then(|| unsafe { patch(base, size, &list) })
}

/// Each site's outcome, for the tests: (image, RVA, why it was refused).
#[cfg(test)]
static OUTCOMES: std::sync::Mutex<Vec<(u64, u32, Option<Refused>)>> =
    std::sync::Mutex::new(Vec::new());

/// Splits the 256-bit stores `list` names in the image mapped at `base`
/// with `size` bytes: (sites patched, sites refused).
///
/// # Safety
/// The image is mapped and no thread runs its code yet.
#[cfg(windows)]
pub unsafe fn patch(base: u64, size: usize, list: &std::path::Path) -> (usize, usize) {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
        PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let Ok(text) = std::fs::read_to_string(list) else {
        tracing::warn!(?list, "VICTORY_PATCH_WIDE: the site list cannot be read");
        return (0, 0);
    };
    let sites: Vec<_> = text.lines().filter_map(parse).collect();
    // SAFETY: the image's headers, mapped with it.
    let headers = unsafe { std::slice::from_raw_parts(base as *const u8, size.min(0x1000)) };
    if !made_from(&text, headers) {
        tracing::warn!(
            ?list,
            image = identity(headers).as_deref().unwrap_or("not a PE32+ image"),
            "VICTORY_PATCH_WIDE: the site list was not made from this image (its first line names another, or none): nothing patched"
        );
        return (0, sites.len());
    }
    let mut pool = std::ptr::null_mut();
    // 64 MiB to 2000 MiB below the image, within reach of rel32, clear of
    // the TLS thunks and console stubs placed right beside it.
    // Below the image first, then above it: an image low in the address
    // space (or one Wine's reservations surround) may have no room below.
    let above = (base + size as u64).next_multiple_of(0x1_0000);
    let below = (64..2000u64).filter_map(|step| base.checked_sub(step << 20));
    let wants = below.chain((0..1936u64).map(|step| above + (step << 20)));
    for want in wants {
        // SAFETY: a plain reservation of free address space, or null.
        pool = unsafe {
            VirtualAlloc(
                want as *const _,
                POOL_SIZE,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if !pool.is_null() {
            break;
        }
    }
    if pool.is_null() {
        tracing::warn!("VICTORY_PATCH_WIDE: no memory near the image for wide store trampolines");
        return (0, sites.len());
    }
    let (mut used, mut patched) = (0usize, 0usize);
    let mut refused: std::collections::BTreeMap<Refused, usize> = Default::default();
    // Pages of the image made writable once each, with their old protection.
    let mut opened: std::collections::BTreeMap<u64, u32> = std::collections::BTreeMap::new();
    let mut placed: Vec<(u32, u32)> = Vec::with_capacity(sites.len());
    let end = base + size as u64;
    for (rva, total, pieces, route) in &sites {
        let site = base + u64::from(*rva);
        let why = (|| -> Result<(), Refused> {
            if site + *total as u64 > end || route.iter().any(|&h| base + u64::from(h) + 5 > end) {
                return Err(Refused::Outside);
            }
            // SAFETY: inside the mapped image (checked above).
            let code = unsafe { std::slice::from_raw_parts(site as *const u8, *total) };
            let at = pool as u64 + used as u64;
            let t = trampoline(base, *rva, *total, pieces, code, at).ok_or(Refused::Bytes)?;
            if used + t.len() > POOL_SIZE {
                return Err(Refused::Full);
            }
            // What is written: (address, bytes), the site's first.
            let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
            if route.is_empty() {
                let rel =
                    i32::try_from(at as i64 - (site as i64 + 5)).map_err(|_| Refused::Reach)?;
                let mut jump = vec![0xe9];
                jump.extend_from_slice(&rel.to_le_bytes());
                jump.resize(*total, 0xcc);
                writes.push((site, jump));
            } else {
                let mut from = site;
                for (k, hop) in route.iter().enumerate() {
                    let to = base + u64::from(*hop);
                    let last = k + 1 == route.len();
                    // SAFETY: padding inside the mapped image (checked above).
                    let pad = unsafe {
                        std::slice::from_raw_parts(to as *const u8, if last { 5 } else { 2 })
                    };
                    let rel =
                        i8::try_from(to as i64 - (from as i64 + 2)).map_err(|_| Refused::Reach)?;
                    if pad.iter().any(|&x| x != 0xcc) {
                        return Err(Refused::Bytes);
                    }
                    let mut j = vec![0xeb, rel as u8];
                    if from == site {
                        j.resize(*total, 0xcc);
                    }
                    writes.push((from, j));
                    if last {
                        let rel = i32::try_from(at as i64 - (to as i64 + 5))
                            .map_err(|_| Refused::Reach)?;
                        let mut j = vec![0xe9];
                        j.extend_from_slice(&rel.to_le_bytes());
                        writes.push((to, j));
                    }
                    from = to;
                }
            }
            // The site's bytes and every hop: none rewritten by another patch.
            let spans = |w: &(u64, Vec<u8>)| (w.0 as usize, w.1.len());
            if writes
                .iter()
                .map(spans)
                .any(|(s, n)| crate::patched::overlaps(s, n))
            {
                return Err(Refused::Overlap);
            }
            for (to, bytes) in &writes {
                for page in (to & !0xfff..=(to + bytes.len() as u64 - 1) & !0xfff).step_by(0x1000) {
                    if let std::collections::btree_map::Entry::Vacant(e) = opened.entry(page) {
                        let mut old = 0;
                        // SAFETY: a page of the mapped image; put back below.
                        if unsafe {
                            VirtualProtect(
                                page as *const _,
                                0x1000,
                                PAGE_EXECUTE_READWRITE,
                                &mut old,
                            )
                        } == 0
                        {
                            return Err(Refused::Protect);
                        }
                        e.insert(old);
                    }
                }
            }
            // Noted only once all of it will be written; checked free above.
            for w in &writes {
                let (s, n) = spans(w);
                crate::patched::take(s, n);
            }
            // SAFETY: the pool is ours and writable; the site and its route
            // lie in the mapped image, opened above, and nothing runs them.
            unsafe {
                std::ptr::copy_nonoverlapping(t.as_ptr(), at as *mut u8, t.len());
                // The hops first, the site last: a thread at the site sees a whole route.
                for (to, bytes) in writes.iter().rev() {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), *to as *mut u8, bytes.len());
                }
            }
            placed.push((used as u32, *rva));
            used = (used + t.len() + 15) & !15;
            Ok(())
        })();
        #[cfg(test)]
        OUTCOMES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((base, *rva, why.err()));
        match why {
            Ok(()) => patched += 1,
            Err(why) => {
                tracing::debug!(
                    rva = format_args!("{rva:#x}"),
                    ?why,
                    "VICTORY_PATCH_WIDE: site left as it is"
                );
                *refused.entry(why).or_default() += 1;
            }
        }
    }
    let refused_all: usize = refused.values().sum();
    // SAFETY: the image pages opened above get their protection back; the pool is ours.
    unsafe {
        for (page, old) in opened {
            let mut was = 0;
            VirtualProtect(page as *const _, 0x1000, old, &mut was);
        }
        if patched == 0 {
            VirtualFree(pool, 0, MEM_RELEASE);
        } else {
            let mut old = 0;
            VirtualProtect(pool, POOL_SIZE, PAGE_EXECUTE_READ, &mut old);
            FlushInstructionCache(GetCurrentProcess(), std::ptr::null(), 0);
        }
    }
    if patched > 0 {
        POOLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Pool {
                start: pool as u64,
                size: POOL_SIZE as u64,
                used: used as u64,
                image: base,
                sites: placed,
            });
    }
    if refused_all > 0 {
        tracing::warn!(refused = ?refused, "VICTORY_PATCH_WIDE: sites left as they are (bytes: not the build the list was made from, or already changed)");
    }
    tracing::info!(
        patched,
        refused = refused_all,
        bytes = used,
        pool = format_args!("{:#x}", pool as u64),
        "wide stores split into 128-bit halves (VICTORY_PATCH_WIDE)"
    );
    (patched, refused_all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_whose_piece_sizes_overflow_is_rejected() {
        // The declared total is the pieces' wrapped sum, so a sum that wraps
        // instead of overflowing would make this line parse.
        let max = usize::MAX;
        let line = format!("1000 {} s{max} s6", max.wrapping_add(6));
        assert!(parse(&line).is_none());
        // Control: an ordinary line.
        assert!(parse("1000 10 s6 c4").is_some());
    }

    #[test]
    fn a_list_line_is_read_or_refused_whole() {
        let (rva, total, pieces, route) = parse("1a5ec31 13 s4 c9").unwrap();
        assert_eq!((rva, total, route.len()), (0x1a5_ec31, 13, 0));
        assert!(matches!(pieces[..], [Piece::Store(4), Piece::Copy(9)]));
        let (_, _, pieces, _) = parse("2d35 13 s4 r7:3:4f00 j2:2e00").unwrap();
        assert!(matches!(
            pieces[..],
            [
                Piece::Store(4),
                Piece::Rip(7, 3, 0x4f00),
                Piece::Jump(2, 0x2e00)
            ]
        ));
        let (_, _, pieces, _) = parse("10 6 s4 k2:5:40").unwrap();
        assert!(matches!(
            pieces[..],
            [Piece::Store(4), Piece::Cond(2, 5, 0x40)]
        ));
        // A route lets a 4-byte site stand alone.
        assert_eq!(parse("10 4 s4 @20,30").unwrap().3, [0x20, 0x30]);
        // Sizes that do not add up, too short without a route, unknown or
        // malformed pieces, a non-ASCII piece.
        for bad in [
            "10 9 s4 c4",
            "10 4 s4",
            "10 5 x5",
            "10 5 s",
            "10 5 j5",
            "zz 5 s5",
            "10 5 é5",
            "",
        ] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn splits_vmovups_rdi() {
        // vmovups [rdi], ymm0
        let s = decode(&[0xc5, 0xfc, 0x11, 0x07]).unwrap();
        let mut c = Vec::new();
        split(&mut c, &s, 0, 0).unwrap();
        // vmovups [rdi+0], xmm0 ; vextractf128 [rdi+16], ymm0, 1
        assert_eq!(
            c,
            [
                0xc4, 0xe1, 0x78, 0x11, 0x87, 0, 0, 0, 0, 0xc4, 0xe3, 0x7d, 0x19, 0x87, 16, 0, 0,
                0, 1
            ]
        );
    }

    #[test]
    fn splits_the_store_of_the_rosetta_stop() {
        // vmovups [r8+0x34], ymm0 = c4 c1 7c 11 40 34 (the store that stopped
        // Rosetta across a protected page).
        let s = decode(&[0xc4, 0xc1, 0x7c, 0x11, 0x40, 0x34]).unwrap();
        let mut c = Vec::new();
        split(&mut c, &s, 0, 0).unwrap();
        // vmovups [r8+0x34], xmm0 ; vextractf128 [r8+0x44], ymm0, 1
        assert_eq!(&c[..9], &[0xc4, 0xc1, 0x78, 0x11, 0x80, 0x34, 0, 0, 0]);
        assert_eq!(&c[9..], &[0xc4, 0xc3, 0x7d, 0x19, 0x80, 0x44, 0, 0, 0, 1]);
    }

    #[test]
    fn splits_r12_with_sib_and_high_register() {
        // vmovups [r12], ymm0 = c4 c1 7c 11 04 24 ; vmovdqu [rax+rcx*8+0x40], ymm9 = c5 7e 7f 4c c8 40
        let s = decode(&[0xc4, 0xc1, 0x7c, 0x11, 0x04, 0x24]).unwrap();
        let mut c = Vec::new();
        split(&mut c, &s, 0, 0).unwrap();
        assert_eq!(&c[..10], &[0xc4, 0xc1, 0x78, 0x11, 0x84, 0x24, 0, 0, 0, 0]);
        let s = decode(&[0xc5, 0x7e, 0x7f, 0x4c, 0xc8, 0x40]).unwrap();
        let mut c = Vec::new();
        split(&mut c, &s, 0, 0).unwrap();
        assert_eq!(
            &c[..10],
            &[0xc4, 0x61, 0x7a, 0x7f, 0x8c, 0xc8, 0x40, 0, 0, 0]
        );
        assert_eq!(
            &c[10..],
            &[0xc4, 0x63, 0x7d, 0x19, 0x8c, 0xc8, 0x50, 0, 0, 0, 1]
        );
    }

    #[test]
    fn a_rip_relative_store_keeps_its_target() {
        // vmovups [rip+0x100], ymm1 = c5 fc 11 0d 00 01 00 00, at 0x1000
        // (its target 0x1108), split into a trampoline at 0x9000.
        let s = decode(&[0xc5, 0xfc, 0x11, 0x0d, 0, 1, 0, 0]).unwrap();
        let mut c = Vec::new();
        split(&mut c, &s, 0x1008, 0x9000).unwrap();
        let low = i32::from_le_bytes(c[5..9].try_into().unwrap());
        assert_eq!(0x9009 + i64::from(low), 0x1108);
        // The high half ends one byte after its displacement (the imm8).
        let high = i32::from_le_bytes(c[14..18].try_into().unwrap());
        assert_eq!(0x9013 + i64::from(high), 0x1118);
        assert_eq!(c.len(), 19);
    }

    #[test]
    fn reaims_a_tls_load_the_redirect_made_a_call() {
        // Series 1.0.2.1 +0x1a5ec31, listed `s4 c9`: vmovups [rdi], ymm0 ;
        // mov rax, gs:[0x58], the load made `call thunk; nop4` by tls.rs
        // (thunks 64 KiB below the image) before the stores were split.
        let base = 0x6fff_ed55_0000u64;
        let (rva, thunk) = (0x1a5_ec31u32, base - 0x1_0000);
        let site = base + u64::from(rva);
        let mut code = vec![0xc5, 0xfc, 0x11, 0x07, 0xe8];
        let rel = i32::try_from(thunk as i64 - (site + 9) as i64).unwrap();
        code.extend(rel.to_le_bytes());
        code.extend(TLS_CALL_FILL);
        let at = base - (64 << 20) + 0xeec10;
        let pieces = [Piece::Store(4), Piece::Copy(9)];
        let t = trampoline(base, rva, 13, &pieces, &code, at).unwrap();
        // The split store, then the call, then the NOP, then the way back.
        assert_eq!(t.len(), 19 + 9 + 5);
        assert_eq!(t[19], 0xe8);
        let call = i32::from_le_bytes(t[20..24].try_into().unwrap());
        assert_eq!((at + 24) as i64 + i64::from(call), thunk as i64);
        assert_eq!(t[24..28], TLS_CALL_FILL);
        let back = i32::from_le_bytes(t[29..33].try_into().unwrap());
        assert_eq!((at + 33) as i64 + i64::from(back), (site + 13) as i64);
        // Copied as it was, the call would have gone to nothing.
        assert_ne!((at + 24) as i64 + i64::from(rel), thunk as i64);
    }

    #[test]
    fn refuses_a_copy_that_became_another_branch() {
        // A copy piece the list made from one plain instruction, found as a
        // jump or a call of another shape when the image is patched (another
        // of our hooks written over it).
        for b in [
            vec![0xc5, 0xfc, 0x11, 0x07, 0xe9, 1, 0, 0, 0],
            vec![
                0xc5, 0xfc, 0x11, 0x07, 0xe8, 1, 0, 0, 0, 0x90, 0x90, 0x90, 0x90,
            ],
            vec![0xc5, 0xfc, 0x11, 0x07, 0xeb, 3, 0x90, 0x90, 0x90],
        ] {
            let pieces = [Piece::Store(4), Piece::Copy(b.len() - 4)];
            assert!(
                trampoline(0x1_4000_0000, 0x1000, b.len(), &pieces, &b, 0x1_3000_0000).is_none()
            );
        }
        // Control: a plain 5-byte copy (mov eax, 1).
        let b = [0xc5, 0xfc, 0x11, 0x07, 0xb8, 1, 0, 0, 0];
        let pieces = [Piece::Store(4), Piece::Copy(5)];
        assert!(trampoline(0x1_4000_0000, 0x1000, 9, &pieces, &b, 0x1_3000_0000).is_some());
    }

    #[test]
    fn refuses_a_store_another_hook_replaced() {
        // `vmovups [rcx+rsi], ymm0` of a renderer port 2 site, then as its
        // hook leaves it: a jump and a NOP.
        let pieces = [Piece::Store(5)];
        let store = [0xc5, 0xfc, 0x11, 0x04, 0x31];
        assert!(trampoline(0x1_4000_0000, 0x1000, 5, &pieces, &store, 0x1_3000_0000).is_some());
        let hooked = [0xe9, 0x10, 0x20, 0x30, 0x40];
        assert!(trampoline(0x1_4000_0000, 0x1000, 5, &pieces, &hooked, 0x1_3000_0000).is_none());
    }

    #[test]
    fn branches_are_checked_against_the_list_and_reaimed() {
        let (base, rva, at) = (0x1_4000_0000u64, 0x1000u32, 0x1_3000_0000u64);
        // vmovups [rdi], ymm0 ; jne +0x10 (to 0x1016) ; listed `s4 k2:5:1016`.
        let code = [0xc5, 0xfc, 0x11, 0x07, 0x75, 0x10];
        let t = trampoline(
            base,
            rva,
            6,
            &[Piece::Store(4), Piece::Cond(2, 5, 0x1016)],
            &code,
            at,
        )
        .unwrap();
        assert_eq!(&t[19..21], &[0x0f, 0x85]);
        let rel = i32::from_le_bytes(t[21..25].try_into().unwrap());
        assert_eq!((at + 25) as i64 + i64::from(rel), (base + 0x1016) as i64);
        // The same bytes listed with another target: another build.
        assert!(
            trampoline(
                base,
                rva,
                6,
                &[Piece::Store(4), Piece::Cond(2, 5, 0x1020)],
                &code,
                at
            )
            .is_none()
        );
        // vmovups [rdi], ymm0 ; jmp +0x20 (to 0x1026), which ends the run.
        let code = [0xc5, 0xfc, 0x11, 0x07, 0xeb, 0x20];
        let t = trampoline(
            base,
            rva,
            6,
            &[Piece::Store(4), Piece::Jump(2, 0x1026)],
            &code,
            at,
        )
        .unwrap();
        assert_eq!(t.len(), 19 + 5);
        let rel = i32::from_le_bytes(t[20..24].try_into().unwrap());
        assert_eq!((at + 24) as i64 + i64::from(rel), (base + 0x1026) as i64);
        assert!(
            trampoline(
                base,
                rva,
                6,
                &[Piece::Store(4), Piece::Jump(2, 0x1030)],
                &code,
                at
            )
            .is_none()
        );
        // lea rax, [rip+0x50] (48 8d 05 50 00 00 00, at 0x1004: to 0x105b).
        let code = [0xc5, 0xfc, 0x11, 0x07, 0x48, 0x8d, 0x05, 0x50, 0, 0, 0];
        let t = trampoline(
            base,
            rva,
            11,
            &[Piece::Store(4), Piece::Rip(7, 3, 0x105b)],
            &code,
            at,
        )
        .unwrap();
        let rel = i32::from_le_bytes(t[22..26].try_into().unwrap());
        assert_eq!((at + 26) as i64 + i64::from(rel), (base + 0x105b) as i64);
        assert!(
            trampoline(
                base,
                rva,
                11,
                &[Piece::Store(4), Piece::Rip(7, 3, 0x1060)],
                &code,
                at
            )
            .is_none()
        );
    }

    #[test]
    fn names_the_site_of_an_address_in_a_pool() {
        let pool = Pool {
            start: 0x1000_0000,
            size: 0x1000,
            used: 0x40,
            image: 0x1_4000_0000,
            sites: vec![(0, 0x2d35), (0x20, 0x1a5_ec31)],
        };
        let site = |rva, into| {
            Some(InPool::Site {
                image: 0x1_4000_0000,
                rva,
                into,
            })
        };
        assert_eq!(pool.find(0x1000_0000), site(0x2d35, 0));
        assert_eq!(pool.find(0x1000_001f), site(0x2d35, 0x1f));
        assert_eq!(pool.find(0x1000_0038), site(0x1a5_ec31, 0x18));
        let unused = Some(InPool::Unused {
            image: 0x1_4000_0000,
        });
        assert_eq!(pool.find(0x1000_0040), unused);
        assert_eq!(pool.find(0x1000_0fff), unused);
        assert_eq!(pool.find(0x1000_1000), None);
        assert_eq!(pool.find(0x0fff_ffff), None);
    }

    #[test]
    fn refuses_loads_and_xmm() {
        assert!(decode(&[0xc5, 0xfc, 0x10, 0x07]).is_none()); // load
        assert!(decode(&[0xc5, 0xf8, 0x11, 0x07]).is_none()); // xmm store
        assert!(decode(&[0xc5, 0xfc, 0x11, 0xc1]).is_none()); // register form
        assert!(decode(&[0xc5, 0xfc, 0x11, 0x07, 0x90]).is_none()); // a byte too many
        assert!(decode(&[0xc5, 0xfc, 0x11, 0x47]).is_none()); // its displacement missing
    }

    #[test]
    fn only_the_prefixes_of_256_bit_stores_are_taken() {
        // (VEX byte 2 of `c5`, opcode): vmovups, vmovupd, vmovaps, vmovntps,
        // vmovdqa, vmovdqu, vmovntdq of ymm0 to [rdi].
        for (p, op) in [
            (0xfc, 0x11),
            (0xfd, 0x11),
            (0xfc, 0x29),
            (0xfc, 0x2b),
            (0xfd, 0x7f),
            (0xfe, 0x7f),
            (0xfd, 0xe7),
        ] {
            assert!(decode(&[0xc5, p, op, 0x07]).is_some(), "{p:#x} {op:#x}");
        }
        // vmovss and vmovsd with L set (ignored: 4 and 8 bytes), 7f and e7
        // without their prefix, a vvvv that is not 1111.
        for (p, op) in [
            (0xfe, 0x11),
            (0xff, 0x11),
            (0xfc, 0x7f),
            (0xff, 0x7f),
            (0xfc, 0xe7),
            (0xfe, 0x29),
            (0xf4, 0x11),
        ] {
            assert!(decode(&[0xc5, p, op, 0x07]).is_none(), "{p:#x} {op:#x}");
        }
    }

    /// PE32+ headers with these TimeDateStamp, SizeOfImage and CheckSum.
    fn headers(time: u32, size: u32, sum: u32) -> Vec<u8> {
        let mut h = vec![0u8; 0x200];
        h[..2].copy_from_slice(b"MZ");
        h[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x88..0x8c].copy_from_slice(&time.to_le_bytes());
        h[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        h[0x98 + 56..0x98 + 60].copy_from_slice(&size.to_le_bytes());
        h[0x98 + 64..0x98 + 68].copy_from_slice(&sum.to_le_bytes());
        h
    }

    #[test]
    fn a_list_made_from_another_image_is_refused_whole() {
        let h = headers(0x6512_34ab, 0x0a00_0000, 0x0bad_f00d);
        let id = "# victory-wide-stores time=651234ab size=0a000000 sum=0badf00d";
        assert_eq!(identity(&h).as_deref(), Some(id));
        assert!(made_from(&format!("{id}\n1000 5 s5\n"), &h));
        // Another build, no header, headers that are not PE32+.
        assert!(!made_from(
            &format!("{id}\n"),
            &headers(0x6512_34ac, 0x0a00_0000, 0x0bad_f00d)
        ));
        assert!(!made_from("1000 5 s5\n", &h));
        assert!(!made_from("", &h));
        let mut pe32 = h.clone();
        pe32[0x98] = 0x0b;
        pe32[0x99] = 0x01;
        assert!(!made_from(&format!("{id}\n"), &pe32));
        // The header is no site.
        assert!(parse(id).is_none());
    }

    /// A trampoline run: the 32 bytes are where the 256-bit store put them,
    /// the code after the site runs, and what does not match is left alone.
    #[cfg(windows)]
    #[test]
    fn a_split_store_writes_what_the_wide_one_did() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc,
        };
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        const SIZE: usize = 0x1_0000;
        // SAFETY: a fresh allocation of our own.
        let image = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                SIZE,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_EXECUTE_READWRITE,
            )
        } as *mut u8;
        assert!(!image.is_null());
        let base = image as u64;
        // +0x100 vmovups ymm0, [rdx] ; +0x104 vmovups [rcx], ymm0 ;
        // +0x108 mov eax, 1 ; +0x10d vmovdqu [rcx+0x20], ymm0 ;
        // +0x112 vzeroupper ; +0x115 ret
        let code = [
            0xc5, 0xfc, 0x10, 0x02, 0xc5, 0xfc, 0x11, 0x01, 0xb8, 1, 0, 0, 0, 0xc5, 0xfe, 0x7f,
            0x41, 0x20, 0xc5, 0xf8, 0x77, 0xc3,
        ];
        // SAFETY: our allocation.
        unsafe {
            std::ptr::write_bytes(image, 0xcc, SIZE);
            let h = headers(1, SIZE as u32, 2);
            std::ptr::copy_nonoverlapping(h.as_ptr(), image, 0xa0);
            std::ptr::copy_nonoverlapping(code.as_ptr(), image.add(0x100), code.len());
        }
        // SAFETY: our allocation.
        let id = identity(unsafe { std::slice::from_raw_parts(image, 0x1000) }).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("sites.txt");
        // The 4-byte store with the mov; the 5-byte one, which another patch
        // of ours is said to have rewritten; a site over bytes the first
        // took; one over bytes that are not a store; one past the image's end.
        std::fs::write(
            &list,
            format!("{id}\n104 9 s4 c5\n10d 5 s5\n108 5 c5\n112 5 s5\nfffe 5 s5\n"),
        )
        .unwrap();
        // Other tests' patches may have noted ranges in memory since freed
        // and handed out again as this allocation: forgotten first.
        crate::patched::forget(base as usize, SIZE);
        assert!(crate::patched::take(base as usize + 0x10f, 1));
        // Told apart from the refusals of the whole list, which count every site.
        let text = std::fs::read_to_string(&list).unwrap();
        // SAFETY: our allocation.
        let mapped = unsafe { std::slice::from_raw_parts(image, 0x1000) };
        assert!(made_from(&text, mapped), "header {id:?} at image {base:#x}");
        // SAFETY: nothing runs the code yet.
        let (patched, refused) = unsafe { patch(base, SIZE, &list) };
        let outcomes: Vec<(u32, Option<Refused>)> = OUTCOMES
            .lock()
            .unwrap()
            .iter()
            .filter(|o| o.0 == base)
            .map(|o| (o.1, o.2))
            .collect();
        eprintln!("image {base:#x}: {patched} patched, {refused} refused: {outcomes:x?}");
        // 0x108: its first byte is now the jump's last (a copy that overlaps
        // the first site), or a branch byte (bytes) if the pool's distance
        // makes it one.
        let want = [
            (0x104, None),
            (0x10d, Some(Refused::Overlap)),
            (0x108, Some(Refused::Overlap)),
            (0x112, Some(Refused::Bytes)),
            (0xfffe, Some(Refused::Outside)),
        ];
        assert_eq!(outcomes.len(), want.len(), "{outcomes:x?}");
        for (got, want) in outcomes.iter().zip(want) {
            let bytes_too = got.0 == 0x108 && got.1 == Some(Refused::Bytes);
            assert!(*got == want || bytes_too, "{outcomes:x?}");
        }
        assert_eq!((patched, refused), (1, 4));
        // SAFETY: our allocation.
        let now = unsafe { std::slice::from_raw_parts(image.add(0x100), code.len()) };
        assert_eq!(now[4], 0xe9);
        assert_eq!(
            &now[13..],
            &code[13..],
            "the refused sites are left as they are"
        );
        assert!(
            crate::patched::overlaps(base as usize + 0x104, 9),
            "the site is noted as taken"
        );
        let rel = i32::from_le_bytes(now[5..9].try_into().unwrap());
        let tramp = (base + 0x109) as i64 + i64::from(rel);
        assert_eq!(
            find(tramp as u64),
            Some(InPool::Site {
                image: base,
                rva: 0x104,
                into: 0
            })
        );
        let src: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
        let mut dst = [0u8; 64];
        // SAFETY: the code reads 32 bytes at rdx and writes 64 at rcx.
        let f: extern "win64" fn(*mut u8, *const u8) -> u32 =
            unsafe { std::mem::transmute(image.add(0x100)) };
        assert_eq!(f(dst.as_mut_ptr(), src.as_ptr()), 1);
        assert_eq!(&dst[..32], &src);
        assert_eq!(&dst[32..], &src);
    }
}

#[cfg(test)]
mod offline {
    use super::*;
    /// `VICTORY_WIDE_IMAGE=<pe file> VICTORY_WIDE_LIST=<list> cargo test
    /// offline -- --nocapture`: every listed site's trampoline built from the
    /// file's bytes, and why those that cannot be are refused.
    #[test]
    fn replay() {
        let (Some(img), Some(list)) = (
            std::env::var_os("VICTORY_WIDE_IMAGE"),
            std::env::var_os("VICTORY_WIDE_LIST"),
        ) else {
            return;
        };
        let file = std::fs::read(img).unwrap();
        let pe = u32::from_le_bytes(file[0x3c..0x40].try_into().unwrap()) as usize;
        let nsec = u16::from_le_bytes(file[pe + 6..pe + 8].try_into().unwrap()) as usize;
        let opt = u16::from_le_bytes(file[pe + 20..pe + 22].try_into().unwrap()) as usize;
        let mut secs = Vec::new();
        for i in 0..nsec {
            let o = pe + 24 + opt + 40 * i;
            let r =
                |k: usize| u32::from_le_bytes(file[o + k..o + k + 4].try_into().unwrap()) as usize;
            secs.push((r(12), r(16), r(20)));
        }
        let at_rva = |rva: usize, n: usize| {
            let &(va, _, roff) = secs.iter().find(|s| s.0 <= rva && rva < s.0 + s.1).unwrap();
            &file[roff + rva - va..roff + rva - va + n]
        };
        let base = 0x6ffff1de0000u64;
        let pool = base - (64 << 20);
        let mut used = 0usize;
        let mut reasons = std::collections::BTreeMap::<String, usize>::new();
        let text = std::fs::read_to_string(list).unwrap();
        for (rva, total, pieces, _route) in text.lines().filter_map(parse) {
            let code = at_rva(rva as usize, total);
            let at = pool + used as u64;
            match trampoline(base, rva, total, &pieces, code, at) {
                Some(t) => used = (used + t.len() + 15) & !15,
                None => {
                    let first = match pieces[0] {
                        Piece::Store(n) => n,
                        _ => 0,
                    };
                    let k = if decode(&code[..first]).is_none() {
                        format!("decode {:02x?}", &code[..first.min(3)])
                    } else {
                        "other".into()
                    };
                    *reasons.entry(k).or_default() += 1;
                }
            }
        }
        let mut r: Vec<_> = reasons.into_iter().collect();
        r.sort_by_key(|x| std::cmp::Reverse(x.1));
        println!("used {used:#x} refused {:?}", &r[..r.len().min(12)]);
    }
}
