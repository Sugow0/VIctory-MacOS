//! The title's `clwb` write-back loops, and their NOPs at load.
//!
//! `clwb` faults under Rosetta: the vectored handler (`crash.rs`) does a
//! whole loop per exception, but each still costs an exception through
//! Wine, and during world load the job threads ran about 15,000 a second
//! (`clwb [rcx]; add rcx, 0x40; cmp rcx, rax; jbe` at `+0x4e4b680`, and
//! seven more). [`nop_clwb_loops`] turns the `clwb` of each loop into a
//! NOP of its length once the image is mapped, before any title thread
//! runs. A PC's memory is coherent, so the write-back had nothing to do;
//! the loop's `add`/`cmp`/`jbe` stay, so its registers end as before.
//!
//! Ported from pragyaangaur/VIctory-macOS PR #152 (`nop_clwb_loops`), gated
//! here on Wine (`wine_get_version`) and `VICTORY_NOP_CLWB`.

/// A `clwb` write-back loop as the title's compilers build it, one cache line a
/// pass (`base` and `limit` any registers):
///
/// ```text
/// top: clwb [base]
///      add  base, 0x40
///      cmp  base, limit
///      jbe  top
/// ```
///
/// An exception costs about a microsecond and the title writes back whole
/// buffers this way while it loads, so the loop is done in one: `clwb` has
/// nothing to do on the PC, `base` ends past `limit` as the last pass leaves it.
pub(crate) struct ClwbLoop {
    pub(crate) base: u8,
    pub(crate) limit: u8,
    /// Bytes from `clwb` to the instruction after `jbe`.
    pub(crate) len: usize,
}
/// The [`ClwbLoop`] `code` starts with, if any.
pub(crate) fn clwb_loop(code: &[u8]) -> Option<ClwbLoop> {
    // clwb [base]: 66 [40|41] 0f ae, ModRM mod 00, reg 6, no SIB or displacement.
    let (rex, at) = match code.get(1)? {
        0x40 | 0x41 => (code[1], 2),
        _ => (0, 1),
    };
    if *code.first()? != 0x66 || code.get(at..at + 2)? != [0x0f, 0xae] {
        return None;
    }
    let modrm = *code.get(at + 2)?;
    let rm = modrm & 7;
    if modrm >> 6 != 0 || (modrm >> 3) & 7 != 6 || rm == 4 || rm == 5 {
        return None;
    }
    let base = rm | (rex & 1) << 3;
    // add base, 0x40: REX.W(+B) 83 /0 ib.
    let add = at + 3;
    if *code.get(add..add + 4)? != [0x48 | base >> 3, 0x83, 0xc0 | (base & 7), 0x40] {
        return None;
    }
    // cmp base, limit: REX.W 39 /r with base in r/m.
    let cmp = add + 4;
    let &[cmp_rex @ 0x48..=0x4f, 0x39, cmp_modrm] = code.get(cmp..cmp + 3)? else {
        return None;
    };
    if cmp_modrm >> 6 != 3 || (cmp_modrm & 7 | (cmp_rex & 1) << 3) != base {
        return None;
    }
    let limit = (cmp_modrm >> 3) & 7 | (cmp_rex & 4) << 1;
    // jbe top.
    let jump = cmp + 3;
    let len = jump + 2;
    if *code.get(jump)? != 0x76 || i64::from(*code.get(jump + 1)? as i8) != -(len as i64) {
        return None;
    }
    (limit != base).then_some(ClwbLoop { base, limit, len })
}

/// The write-back loops retail 1.0.2.1 has (image `b99f876b74d4a77f`:
/// `+0x4e4b680`, `+0x4ecc84a`, `+0x50f75f5`, `+0x532f1b0`, `+0x532f1f0`,
/// `+0x532f230`, `+0x532f280`, `+0x532f2ba`). Another count is another
/// build, or a pattern that is no loop, and nothing is patched.
pub(crate) const CLWB_LOOPS: usize = 8;

/// Where `code` has a [`clwb_loop`]: the offset of its `clwb` and that
/// instruction's length (66, an optional REX, 0f ae, ModRM).
pub(crate) fn clwb_loop_sites(code: &[u8]) -> Vec<(usize, usize)> {
    (0..code.len())
        .filter(|&at| code[at] == 0x66)
        .filter_map(|at| {
            clwb_loop(&code[at..(at + 32).min(code.len())])?;
            Some((
                at,
                if matches!(code[at + 1], 0x40 | 0x41) {
                    5
                } else {
                    4
                },
            ))
        })
        .collect()
}

/// The NOP that takes the place of a `clwb` of `len` bytes.
pub(crate) fn nop_of(len: usize) -> Option<&'static [u8]> {
    match len {
        4 => Some(&[0x0f, 0x1f, 0x40, 0x00]),       // nop dword [rax+0]
        5 => Some(&[0x0f, 0x1f, 0x44, 0x00, 0x00]), // nop dword [rax+rax+0]
        _ => None,
    }
}

/// Puts the NOP over the `clwb` that `site` starts with, if `site` (the
/// loop's bytes, as many as there are up to 32) still starts with a
/// [`clwb_loop`] of `len`: the bytes are checked again at the moment of
/// the write.
pub(crate) fn nop_clwb(site: &mut [u8], len: usize) -> bool {
    let Some(nop) = nop_of(len) else {
        return false;
    };
    let now = if matches!(site.get(1), Some(0x40 | 0x41)) {
        5
    } else {
        4
    };
    if now != len || clwb_loop(site).is_none() {
        return false;
    }
    site[..len].copy_from_slice(nop);
    true
}

/// Whether the loops are made NOPs: `VICTORY_NOP_CLWB=0` never, `1`
/// always, anything else under Wine only (where Rosetta runs the title).
pub(crate) fn wanted(switch: Option<&str>, wine: impl FnOnce() -> bool) -> bool {
    match switch.map(str::trim) {
        Some("0") => false,
        Some("1") => true,
        _ => wine(),
    }
}

/// Whether we run under Wine: its ntdll exports `wine_get_version`.
#[cfg(windows)]
fn on_wine() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    // SAFETY: plain lookups in ntdll, always loaded.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
    }
}

/// Under Wine (or with `VICTORY_NOP_CLWB=1`), turns the `clwb` of every
/// [`clwb_loop`] in the executable sections of the image mapped at `base`
/// into a NOP of its length, and returns how many, or `None` when it was
/// not asked for. Nothing is patched, and `Some(0)` returned, unless
/// exactly [`CLWB_LOOPS`] loops are found; the count found is logged then.
///
/// # Safety
/// `base` is a mapped image described by `info`, none of whose code runs
/// yet.
#[cfg(windows)]
pub unsafe fn nop_clwb_loops(base: usize, info: &victory_pe::ImageInfo) -> Option<usize> {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    const EXECUTE: u32 = 0x2000_0000;
    if !wanted(std::env::var("VICTORY_NOP_CLWB").ok().as_deref(), on_wine) {
        return None;
    }
    let mut sites = Vec::new();
    for s in info
        .sections
        .iter()
        .filter(|s| s.characteristics & EXECUTE != 0)
    {
        let start = base + s.rva as usize;
        let len = s.virtual_size.min(s.file_size) as usize;
        // SAFETY: the mapped section, readable code (the caller's promise).
        let code = unsafe { std::slice::from_raw_parts(start as *const u8, len) };
        sites.extend(
            clwb_loop_sites(code)
                .into_iter()
                .map(|(at, n)| (start + at, n, (len - at).min(32))),
        );
    }
    if sites.len() != CLWB_LOOPS {
        tracing::warn!(
            found = sites.len(),
            expected = CLWB_LOOPS,
            "clwb write-back loops are not made NOPs: this is not the build they were counted in"
        );
        return Some(0);
    }
    let mut patched = 0;
    for (site, n, room) in sites {
        let mut old = 0;
        // SAFETY: the title's code, writable for the moment of the write,
        // before any title thread runs; `room` bytes of it are mapped.
        unsafe {
            if VirtualProtect(site as *const _, n, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
                tracing::warn!(
                    at = format_args!("{site:#x}"),
                    "clwb loop left as it is: VirtualProtect failed"
                );
                continue;
            }
            if nop_clwb(std::slice::from_raw_parts_mut(site as *mut u8, room), n) {
                patched += 1;
            }
            VirtualProtect(site as *const _, n, old, &mut old);
        }
    }
    // SAFETY: flushes the whole instruction cache of our process.
    unsafe { FlushInstructionCache(GetCurrentProcess(), std::ptr::null(), 0) };
    Some(patched)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title's loop at `+0x4e4b680` (#135), behind a lea and a lone
    /// `clflushopt` that is no loop (ported from PR #152's test).
    const CODE: [u8; 23] = [
        0x48, 0x8d, 0x44, 0x0a, 0xff, // lea rax, [rdx+rcx-1]
        0x66, 0x0f, 0xae, 0x39, // clflushopt [rcx], no loop
        0x66, 0x0f, 0xae, 0x31, // top: clwb [rcx]
        0x48, 0x83, 0xc1, 0x40, //      add rcx, 0x40
        0x48, 0x39, 0xc1, //            cmp rcx, rax
        0x76, 0xf3, //                  jbe top
        0xc3,
    ];
    /// With REX.B: clwb [r9]; add r9, 0x40; cmp r9, rax; jbe top.
    const REX: [u8; 14] = [
        0x66, 0x41, 0x0f, 0xae, 0x31, 0x49, 0x83, 0xc1, 0x40, 0x49, 0x39, 0xc1, 0x76, 0xf2,
    ];

    #[test]
    fn clwb_write_back_loops_are_found_for_nops() {
        assert_eq!(clwb_loop_sites(&CODE), vec![(9, 4)]);
        assert_eq!(clwb_loop_sites(&REX), vec![(0, 5)]);
        // A loop cut off at the end of the section is none.
        assert_eq!(clwb_loop_sites(&CODE[..21]), vec![]);
    }

    #[test]
    fn the_nop_takes_only_the_clwb() {
        let mut code = CODE;
        assert!(nop_clwb(&mut code[9..], 4));
        assert_eq!(code[9..13], [0x0f, 0x1f, 0x40, 0x00]);
        assert_eq!(code[..9], CODE[..9], "the clflushopt and the lea stay");
        assert_eq!(code[13..], CODE[13..], "add, cmp and jbe stay");
        // Once made a NOP there is no loop to find, nor to patch again.
        assert_eq!(clwb_loop_sites(&code), vec![]);
        assert!(!nop_clwb(&mut code[9..], 4));

        let mut rex = REX;
        assert!(nop_clwb(&mut rex, 5));
        assert_eq!(rex[..5], [0x0f, 0x1f, 0x44, 0x00, 0x00]);
        assert_eq!(rex[5..], REX[5..]);
    }

    #[test]
    fn a_site_that_is_not_the_loop_found_is_left_alone() {
        // The wrong length for the bytes, no loop at all, an unknown length.
        let mut code = CODE;
        assert!(!nop_clwb(&mut code[9..], 5));
        assert!(!nop_clwb(&mut code[5..], 4));
        assert!(!nop_clwb(&mut code[9..], 3));
        assert_eq!(code, CODE);
    }

    #[test]
    fn the_nops_are_wanted_under_wine_unless_switched() {
        assert!(wanted(None, || true));
        assert!(!wanted(None, || false));
        assert!(!wanted(Some("0"), || true));
        assert!(wanted(Some("1"), || false));
        assert!(wanted(Some("yes"), || true));
        assert!(!wanted(Some("yes"), || false));
    }
}
