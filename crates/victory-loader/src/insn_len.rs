//! The length of an x86-64 instruction, for walking a function's code from its start
//! (`masked.rs` keeps only byte matches that are instructions of their own).
//!
//! Legacy, two-byte (0F), three-byte (0F38, 0F3A) and VEX encodings, which are what a Zen 2
//! title has. What is not one of those, or is invalid in 64-bit mode (EVEX, XOP, 3DNow!, the
//! opcodes 64-bit mode removed), is `None`: the caller then trusts nothing of that function.

/// One decoded instruction: its length, and whether legacy prefixes (segment, address size)
/// come before a VEX prefix, at `vex_at` bytes from the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Insn {
    pub len: usize,
    /// Where the VEX prefix starts, for a VEX instruction.
    pub vex_at: Option<usize>,
}

/// The bytes a ModRM operand takes from `at` (ModRM, SIB, displacement).
fn modrm_len(code: &[u8], at: usize) -> Option<usize> {
    let modrm = *code.get(at)?;
    let (md, rm) = (modrm >> 6, modrm & 7);
    if md == 3 {
        return Some(1);
    }
    let mut len = 1;
    let mut base5 = false;
    if rm == 4 {
        base5 = code.get(at + 1)? & 7 == 5;
        len += 1;
    }
    len += match md {
        1 => 1,
        2 => 4,
        _ if rm == 5 || base5 => 4,
        _ => 0,
    };
    Some(len)
}

/// The instruction at the start of `code`.
pub(crate) fn insn(code: &[u8]) -> Option<Insn> {
    let mut at = 0;
    let (mut opsize, mut addr32, mut rep) = (false, false, 0u8);
    // Legacy prefixes, then at most one REX right before the opcode.
    loop {
        match *code.get(at)? {
            0x66 => opsize = true,
            0x67 => addr32 = true,
            p @ (0xf2 | 0xf3) => rep = p,
            0xf0 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
            _ => break,
        }
        at += 1;
        if at > 14 {
            return None;
        }
    }
    let legacy = at;
    let (mut rex, mut rex_w) = (false, false);
    if let r @ 0x40..=0x4f = *code.get(at)? {
        rex = true;
        rex_w = r & 8 != 0;
        at += 1;
    }
    let op = *code.get(at)?;
    at += 1;
    let z = if opsize { 2 } else { 4 };
    let end = |at: usize, modrm: bool, imm: usize| -> Option<Insn> {
        let len = at + if modrm { modrm_len(code, at)? } else { 0 } + imm;
        (len <= 15 && len <= code.len()).then_some(Insn { len, vex_at: None })
    };
    match op {
        // VEX: no legacy 66/F2/F3/F0 nor REX may come before it.
        0xc4 | 0xc5 => {
            if rex || opsize || rep != 0 || code[..legacy].contains(&0xf0) {
                return None;
            }
            let vex_at = at - 1;
            let (map, len) = if op == 0xc5 {
                (1, 2)
            } else {
                (*code.get(at)? & 0x1f, 3)
            };
            let opcode = *code.get(vex_at + len)?;
            let at = vex_at + len + 1;
            let (modrm, imm) = match map {
                1 => (
                    opcode != 0x77,
                    usize::from(matches!(opcode, 0x70..=0x73 | 0xc2 | 0xc4..=0xc6)),
                ),
                2 => (true, 0),
                3 => (true, 1),
                _ => return None,
            };
            let mut i = end(at, modrm, imm)?;
            i.vex_at = Some(vex_at);
            Some(i)
        }
        0x0f => two_byte(code, at, opsize, rep, end),
        // Invalid in 64-bit mode, EVEX (62) and XOP (8F with ModRM.reg not 0).
        0x06 | 0x07 | 0x0e | 0x16 | 0x17 | 0x1e | 0x1f | 0x27 | 0x2f | 0x37 | 0x3f | 0x60
        | 0x61 | 0x62 | 0x82 | 0x9a | 0xce | 0xd4 | 0xd5 | 0xd6 | 0xea => None,
        0x8f if code.get(at)? >> 3 & 7 != 0 => None,
        0x00..=0x3f => match op & 7 {
            0..=3 => end(at, true, 0),
            4 => end(at, false, 1),
            5 => end(at, false, z),
            _ => None,
        },
        0x50..=0x5f | 0x6c..=0x6f | 0x90..=0x99 | 0x9b..=0x9f | 0xa4..=0xa7 | 0xaa..=0xaf => {
            end(at, false, 0)
        }
        0xc3
        | 0xc9
        | 0xcb
        | 0xcc
        | 0xcf
        | 0xd7
        | 0xec..=0xef
        | 0xf1
        | 0xf4
        | 0xf5
        | 0xf8..=0xfd => end(at, false, 0),
        0x63 | 0x84..=0x8f | 0xd0..=0xd3 | 0xd8..=0xdf | 0xfe | 0xff => end(at, true, 0),
        0x68 => end(at, false, z),
        0x69 | 0x81 | 0xc7 => end(at, true, z),
        0x6a | 0x70..=0x7f | 0xa8 | 0xb0..=0xb7 | 0xcd | 0xe0..=0xe7 | 0xeb => end(at, false, 1),
        0x6b | 0x80 | 0x83 | 0xc0 | 0xc1 | 0xc6 => end(at, true, 1),
        0xa0..=0xa3 => end(at, false, if addr32 { 4 } else { 8 }),
        0xa9 => end(at, false, z),
        0xb8..=0xbf => end(at, false, if rex_w { 8 } else { z }),
        0xc2 | 0xca => end(at, false, 2),
        0xc8 => end(at, false, 3),
        0xe8 | 0xe9 => end(at, false, 4),
        0xf6 | 0xf7 => {
            let reg = code.get(at)? >> 3 & 7;
            let imm = if reg > 1 {
                0
            } else if op == 0xf6 {
                1
            } else {
                z
            };
            end(at, true, imm)
        }
        _ => None,
    }
}

/// The 0F map (and 0F38, 0F3A), opcode byte at `at`.
fn two_byte(
    code: &[u8],
    at: usize,
    opsize: bool,
    rep: u8,
    end: impl Fn(usize, bool, usize) -> Option<Insn>,
) -> Option<Insn> {
    let op = *code.get(at)?;
    let at = at + 1;
    match op {
        0x38 => {
            code.get(at)?;
            end(at + 1, true, 0)
        }
        0x3a => {
            code.get(at)?;
            end(at + 1, true, 1)
        }
        // Undefined, 3DNow! (0F 0F) and SSE5's old 7A/7B.
        0x04
        | 0x0a
        | 0x0c
        | 0x0f
        | 0x24..=0x27
        | 0x36
        | 0x39
        | 0x3b..=0x3f
        | 0x7a
        | 0x7b
        | 0xa6
        | 0xa7 => None,
        0x05..=0x09
        | 0x0b
        | 0x0e
        | 0x30..=0x35
        | 0x37
        | 0x77
        | 0xa0..=0xa2
        | 0xa8..=0xaa
        | 0xc8..=0xcf => end(at, false, 0),
        0x80..=0x8f => end(at, false, 4),
        // SSE4a `extrq`/`insertq` with two immediates.
        0x78 if opsize || rep == 0xf2 => end(at, true, 2),
        0x70..=0x73 | 0xa4 | 0xac | 0xba | 0xc2 | 0xc4..=0xc6 => end(at, true, 1),
        _ => end(at, true, 0),
    }
}

/// The starts of the instructions from `code`'s start to its end, if every byte decodes and
/// the last instruction ends exactly at the end.
pub(crate) fn starts(code: &[u8]) -> Option<Vec<(usize, Insn)>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < code.len() {
        let i = insn(&code[at..])?;
        out.push((at, i));
        at += i.len;
    }
    (at == code.len()).then_some(out)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn len(code: &[u8]) -> Option<usize> {
        insn(code).map(|i| i.len)
    }

    #[test]
    fn lengths_of_common_instructions() {
        let cases: &[(&[u8], usize)] = &[
            (&[0xc3], 1),                                        // ret
            (&[0x48, 0x89, 0x5c, 0x24, 0x08], 5),                // mov [rsp+8], rbx
            (&[0x48, 0x83, 0xec, 0x28], 4),                      // sub rsp, 0x28
            (&[0x48, 0x8d, 0x05, 1, 2, 3, 4], 7),                // lea rax, [rip+x]
            (&[0xe8, 0, 0, 0, 0], 5),                            // call rel32
            (&[0x0f, 0x84, 0, 0, 0, 0], 6),                      // je rel32
            (&[0x48, 0xb8, 1, 2, 3, 4, 5, 6, 7, 8], 10),         // mov rax, imm64
            (&[0xb8, 1, 2, 3, 4], 5),                            // mov eax, imm32
            (&[0x66, 0xb8, 1, 2], 4),                            // mov ax, imm16
            (&[0xc7, 0x44, 0x24, 0x20, 1, 2, 3, 4], 8),          // mov dword [rsp+0x20], imm32
            (&[0xf6, 0x41, 0x10, 0x01], 4),                      // test byte [rcx+0x10], 1
            (&[0xf7, 0xd8], 2),                                  // neg eax
            (&[0x0f, 0x1f, 0x44, 0x00, 0x00], 5),                // nop dword [rax+rax]
            (&[0x66, 0x0f, 0x3a, 0x0f, 0xc1, 0x08], 6),          // palignr xmm0, xmm1, 8
            (&[0x66, 0x0f, 0x38, 0x00, 0xc1], 5),                // pshufb
            (&[0xc5, 0xfc, 0x11, 0x00], 4),                      // vmovups [rax], ymm0
            (&[0xc5, 0xf8, 0x77], 3),                            // vzeroupper
            (&[0xc4, 0xe2, 0x45, 0x8e, 0x30], 5),                // vpmaskmovd [rax], ymm7, ymm6
            (&[0xc4, 0xe2, 0x4d, 0x8e, 0x04, 0x90], 6),          // vpmaskmovd [rax+rdx*4]
            (&[0xc4, 0xe3, 0x7d, 0x19, 0x49, 0x10, 0x01], 7),    // vextractf128 [rcx+16], ymm1, 1
            (&[0xc5, 0xf9, 0x70, 0xc1, 0x1b], 5),                // vpshufd xmm0, xmm1, 0x1b
            (&[0x65, 0x48, 0x8b, 0x04, 0x25, 0x58, 0, 0, 0], 9), // mov rax, gs:[0x58]
            (&[0xf3, 0x0f, 0x1e, 0xfa], 4),                      // endbr64
            (&[0xf0, 0x48, 0x0f, 0xb1, 0x11], 5),                // lock cmpxchg [rcx], rdx
        ];
        for (code, want) in cases {
            assert_eq!(len(code), Some(*want), "{code:02x?}");
        }
    }

    #[test]
    fn what_64_bit_mode_lacks_is_refused() {
        assert_eq!(len(&[0x62, 0xf1, 0x7c, 0x48, 0x11, 0x00]), None); // EVEX
        assert_eq!(len(&[0x8f, 0xe9, 0x78, 0xc2, 0xc0]), None); // XOP
        assert_eq!(len(&[0x0f, 0x0f, 0xc1, 0xb4]), None); // 3DNow!
        assert_eq!(len(&[0x06]), None);
        // A REX or 66 before VEX is invalid.
        assert_eq!(len(&[0x48, 0xc5, 0xfc, 0x11, 0x00]), None);
        // Cut short.
        assert_eq!(len(&[0xe8, 0, 0]), None);
    }

    #[test]
    fn a_segment_prefix_before_vex_is_told() {
        let i = insn(&[0x64, 0xc4, 0xe2, 0x45, 0x8e, 0x30]).unwrap();
        assert_eq!((i.len, i.vex_at), (6, Some(1)));
    }

    #[test]
    fn starts_walk_to_the_end_or_refuse() {
        // mov eax, imm32 (whose bytes hide a vpmaskmovd), xor al, al, ret.
        let code = [0xb8, 0xc4, 0xe2, 0x45, 0x8e, 0x30, 0xc0, 0xc3];
        let at: Vec<usize> = starts(&code).unwrap().iter().map(|(a, _)| *a).collect();
        assert_eq!(at, [0, 5, 7]);
        // The last instruction runs past the end.
        assert_eq!(starts(&code[..6]), None);
    }
}
