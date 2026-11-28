//! The title's traps a thread goes on behind (`VICTORY_STEP_OVER`).
//!
//! Ported from pragyaangaur/VIctory-macOS PR #284 (itself a port of
//! Ahmet-Dedeler/VIctory 0d216a5d, alexodee's handler idea from
//! fxceless/victory-fork#378), narrowed to the one site we checked.
//!
//! The title asserts with `int3`, which ends it without a debugger. One of
//! those traps sits on a function's own way out: the allocator of a
//! fixed-size pool (the instance lists', Marvisak/VIctory#698) traps when
//! the pool is full and then returns NULL. In retail 1.0.2.1 (image
//! `b99f876b74d4a77f`) that is `+0x5149aec`, `cc 31 ff` (`int3; xor edi,
//! edi`), and its caller at `+0xa5db22` already tests the result
//! (`test rax, rax; je`). On the console the trap is a no-op; here the pool
//! fills when frames take seconds, about 20 minutes into the opening, and
//! the process ends. Stepping over that one byte gives the caller its NULL:
//! what asked for the element is not drawn, and the run goes on.
//!
//! This module is the portable part (which sites, what their bytes must
//! be); the vectored handler that resumes the thread is in `crash.rs`.

/// The full-pool traps `pool` names, as offsets into a title image. Each is
/// stepped over only where its bytes are an `int3` followed by an
/// instruction zeroing a register ([`Check::PoolTrap`]).
pub(crate) const POOL_TRAPS: [usize; 1] = [0x5149aec];

/// What the bytes at a site must be before the thread is moved past them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// `int3`, then `xor r, r` (the pool allocators' NULL).
    PoolTrap,
    /// `int3` (an offset named in the switch: the caller's choice).
    Int3,
}

/// One place to step over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) offset: usize,
    pub(crate) check: Check,
}

/// Bytes [`matches`] reads at a site: `int3`, a REX prefix, opcode, ModRM.
pub(crate) const SITE_BYTES: usize = 4;

/// The sites `VICTORY_STEP_OVER` asks for, and the words of it that are
/// neither `pool` nor a hex offset.
///
/// Unset is `pool`, the default: the known trap is stepped over only where
/// its bytes are checked, and without it the run has already ended. `0`,
/// `off` or `none` steps over nothing. Otherwise a comma list of `pool`
/// and hex offsets (`0x` optional).
pub(crate) fn sites(value: Option<&str>) -> (Vec<Site>, Vec<String>) {
    let value = value.unwrap_or("pool").trim();
    let mut sites: Vec<Site> = Vec::new();
    let mut unknown = Vec::new();
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "0" | "off" | "none"
    ) {
        return (sites, unknown);
    }
    let mut add = |site: Site| {
        if !sites.iter().any(|s| s.offset == site.offset) {
            sites.push(site);
        }
    };
    for word in value.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        if word.eq_ignore_ascii_case("pool") {
            for offset in POOL_TRAPS {
                add(Site {
                    offset,
                    check: Check::PoolTrap,
                });
            }
            continue;
        }
        let digits = word
            .strip_prefix("0x")
            .or_else(|| word.strip_prefix("0X"))
            .unwrap_or(word);
        match usize::from_str_radix(digits, 16) {
            Ok(offset) if offset != 0 => add(Site {
                offset,
                check: Check::Int3,
            }),
            _ => unknown.push(word.to_owned()),
        }
    }
    (sites, unknown)
}

/// Whether `bytes` (at least [`SITE_BYTES`] of them, from the site) are
/// what `check` asks for.
pub(crate) fn matches(check: Check, bytes: &[u8]) -> bool {
    match (check, bytes) {
        (Check::Int3, [0xcc, ..]) => true,
        (Check::PoolTrap, [0xcc, behind @ ..]) => zeroes_a_register(behind),
        _ => false,
    }
}

/// Whether `behind` starts with `xor r, r` of one register with itself
/// (`31 /r` or `33 /r` with both ModRM fields the same, under a REX prefix
/// that extends both or neither).
pub(crate) fn zeroes_a_register(behind: &[u8]) -> bool {
    let (rex, rest) = match behind {
        [rex @ 0x40..=0x4f, rest @ ..] => (*rex, rest),
        rest => (0, rest),
    };
    // REX.R and REX.B must agree for the two fields to name one register.
    if (rex >> 2 & 1) != (rex & 1) {
        return false;
    }
    matches!(rest, [0x31 | 0x33, modrm, ..] if modrm >> 6 == 3 && (modrm >> 3 & 7) == (modrm & 7))
}

/// Whether the `n`th step over a site is logged: the first three, then at
/// powers of two.
pub(crate) fn worth_logging(n: u64) -> bool {
    n <= 3 || n.is_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL: Site = Site {
        offset: 0x5149aec,
        check: Check::PoolTrap,
    };

    #[test]
    fn unset_is_the_known_pool_trap_and_off_is_nothing() {
        assert_eq!(sites(None), (vec![POOL], vec![]));
        for off in ["0", "off", "NONE", "", "  "] {
            assert_eq!(sites(Some(off)), (vec![], vec![]), "{off:?}");
        }
    }

    #[test]
    fn a_list_names_pool_and_offsets_once_each() {
        let int3 = |offset| Site {
            offset,
            check: Check::Int3,
        };
        assert_eq!(
            sites(Some("pool, 63966f,0x1234,Pool,5149aec")),
            (vec![POOL, int3(0x63966f), int3(0x1234)], vec![])
        );
        // An offset alone is not the pool's check, and words that are no
        // offset are given back.
        assert_eq!(
            sites(Some("5149aec,zz,0")),
            (vec![int3(0x5149aec)], vec!["zz".to_owned(), "0".to_owned()])
        );
    }

    #[test]
    fn the_pool_check_wants_int3_then_a_zeroing_xor() {
        // The title's own bytes at +0x5149aec and +0x63966f.
        assert!(matches(Check::PoolTrap, &[0xcc, 0x31, 0xff, 0x48]));
        assert!(matches(Check::PoolTrap, &[0xcc, 0x31, 0xdb, 0xeb]));
        // xor rax, rax; xor r8d, r8d (REX.R and REX.B); xor eax, eax by 33.
        assert!(matches(Check::PoolTrap, &[0xcc, 0x48, 0x31, 0xc0]));
        assert!(matches(Check::PoolTrap, &[0xcc, 0x45, 0x31, 0xc0]));
        assert!(matches(Check::PoolTrap, &[0xcc, 0x33, 0xc0, 0x90]));
        // Two registers, a memory operand, REX.B alone, no int3, too short.
        assert!(!matches(Check::PoolTrap, &[0xcc, 0x31, 0xc8, 0x90])); // xor eax, ecx
        assert!(!matches(Check::PoolTrap, &[0xcc, 0x31, 0x07, 0x90])); // xor [rdi], eax
        assert!(!matches(Check::PoolTrap, &[0xcc, 0x41, 0x31, 0xc0])); // xor r8d, eax
        assert!(!matches(Check::PoolTrap, &[0x90, 0x31, 0xff, 0x48]));
        assert!(!matches(Check::PoolTrap, &[0xcc, 0x31]));
        assert!(!matches(Check::PoolTrap, &[]));
        // A named offset only needs the int3.
        assert!(matches(Check::Int3, &[0xcc, 0x90, 0x90, 0x90]));
        assert!(!matches(Check::Int3, &[0xcd, 0x03, 0x90, 0x90]));
    }

    #[test]
    fn steps_are_logged_three_times_then_at_powers_of_two() {
        let logged: Vec<u64> = (1..=40).filter(|&n| worth_logging(n)).collect();
        assert_eq!(logged, [1, 2, 3, 4, 8, 16, 32]);
    }
}
