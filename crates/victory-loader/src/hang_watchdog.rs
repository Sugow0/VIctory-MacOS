//! The title's hang watchdog (`VICTORY_HANG_WATCHDOG`; off under Wine, on
//! elsewhere): three jumps to its crash path made two-byte no-ops at load.
//!
//! What the title does (game_scarlett_master.exe 1.0.2.1, BLAKE3
//! [`BUILD`], image offsets): a thread of its own checks once a second
//! that each registered scoped timer is younger than its limit (300 s), and
//! that the main loop has written its heartbeat within 30 checks. If not,
//! it calls a hook, then writes to address 0 (`mov dword [0], 0` at
//! +0x2215c35, an `int3` after it): the process ends there. Under Rosetta
//! and Wine the world load outlasts the 30 s, and the title ended itself
//! while loading.
//!
//! - +0x2215bdc `jae` (`73 2e`): a timer older than its limit, to the
//!   crash (+0x2215c0c, then +0x2215c16).
//! - +0x2215c70 and +0x2215d84 `je` (`74 04`): where each of the loop's
//!   two forms has counted its 30 checks down without a heartbeat, to the
//!   crash (`xor ebx, ebx ; jmp +0x2215c16`, and the same path's copy).
//!
//! Each becomes `66 90` (a two-byte nop): the check falls through to the
//! next timer or the next second. Nothing is written unless the build is
//! [`BUILD`] and all three sites hold their bytes (pragyaangaur/VIctory-macOS
//! pull request #200, `VICTORY_NO_HANG_WATCHDOG=1`, brought over).

/// BLAKE3 of the one executable these offsets are of (Series 1.0.2.1).
pub const BUILD: &str = "b99f876b74d4a77f03af5f878223b27bd647b5c35a79d5decd9f716504239e93";

/// What goes at each site: a two-byte nop.
pub const NOP2: [u8; 2] = [0x66, 0x90];

/// A jump to the crash path: its offset in the image and its bytes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    pub at: usize,
    pub bytes: [u8; 2],
}

/// The three jumps of [`BUILD`].
pub const SITES: [Site; 3] = [
    // vucomiss xmm8, xmm7 ; jae +0x2215c0c (a scoped timer past its limit)
    Site { at: 0x2215bdc, bytes: [0x73, 0x2e] },
    // je +0x2215c76 (xor ebx, ebx ; jmp to the crash): 30 checks without a heartbeat
    Site { at: 0x2215c70, bytes: [0x74, 0x04] },
    // dec ebp ; je +0x2215d8a: the loop's other form, the same count
    Site { at: 0x2215d84, bytes: [0x74, 0x04] },
];

/// `VICTORY_HANG_WATCHDOG`: `Some(true)` keeps the title's watchdog
/// (`1`, `on`), `Some(false)` turns it off (`0`, `off`); unset, `None` (the
/// default: off under Wine, on elsewhere). Anything else is an error.
pub fn parse_switch(spec: Option<&str>) -> Result<Option<bool>, String> {
    match spec.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") => Ok(None),
        Some("1" | "on") => Ok(Some(true)),
        Some("0" | "off") => Ok(Some(false)),
        Some(other) => Err(format!("VICTORY_HANG_WATCHDOG={other}: expected 0 (off), 1 (on), off or on")),
    }
}

/// Whether the watchdog is kept: the switch if given, else on but under Wine.
pub fn keep_watchdog(asked: Option<bool>, wine: bool) -> bool {
    asked.unwrap_or(!wine)
}

/// Why the title is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Another build of the title.
    OtherBuild,
    /// A site lies past the image's end.
    Outside(usize),
    /// A site's bytes are not the ones expected.
    Differs(usize),
}

/// Checks that `hash` (hex) is [`BUILD`] and every site of `sites` holds
/// its bytes in `image` (the mapped image, from its base). Nothing is
/// written by this.
pub fn check(hash: &str, image: &[u8], sites: &[Site]) -> Result<(), Refused> {
    if !hash.eq_ignore_ascii_case(BUILD) {
        return Err(Refused::OtherBuild);
    }
    for site in sites {
        let there = site.at.checked_add(2).and_then(|end| image.get(site.at..end)).ok_or(Refused::Outside(site.at))?;
        if there != site.bytes {
            return Err(Refused::Differs(site.at));
        }
    }
    Ok(())
}

/// Turns the title's watchdog off if `VICTORY_HANG_WATCHDOG` (or its
/// default) asks it, in the image of `size` bytes at `base` whose file has
/// BLAKE3 `hash`. One line in the log says what was done.
///
/// # Safety
/// The image stays mapped and no thread runs its code yet.
#[cfg(windows)]
pub unsafe fn apply_asked(hash: &str, base: usize, size: usize) {
    let spec = std::env::var("VICTORY_HANG_WATCHDOG").ok();
    let asked = match parse_switch(spec.as_deref()) {
        Ok(asked) => asked,
        Err(why) => {
            println!("hang watchdog: {why}; the title's left on");
            return;
        }
    };
    let wine = on_wine();
    if keep_watchdog(asked, wine) {
        let why = if asked.is_some() { "VICTORY_HANG_WATCHDOG=1" } else { "not under Wine" };
        println!("hang watchdog: the title's left on ({why})");
        return;
    }
    let why = if asked.is_some() { "VICTORY_HANG_WATCHDOG=0" } else { "under Wine" };
    // SAFETY: the mapped image, `size` bytes from its base.
    let image = unsafe { std::slice::from_raw_parts(base as *const u8, size) };
    match check(hash, image, &SITES) {
        Ok(()) => {}
        Err(Refused::OtherBuild) => {
            println!("hang watchdog: not turned off ({why}): not the build its offsets are of; the title's left alone");
            return;
        }
        Err(Refused::Outside(at) | Refused::Differs(at)) => {
            println!("hang watchdog: not turned off ({why}): the bytes at {at:#x} are not the ones expected; the title's left alone");
            return;
        }
    }
    // SAFETY: inside the image, checked; nothing runs it yet.
    match unsafe { write(base, &SITES) } {
        Ok(()) => println!("hang watchdog: off ({why}; {} jumps to its crash path made nops)", SITES.len()),
        Err(at) => println!("hang watchdog: not turned off ({why}): Windows would not make {at:#x} writable"),
    }
}

/// Puts [`NOP2`] at each site; the offset of one that could not be made
/// writable stops it.
///
/// # Safety
/// Each site lies inside the image at `base`, which no thread runs yet.
#[cfg(windows)]
unsafe fn write(base: usize, sites: &[Site]) -> Result<(), usize> {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    for site in sites {
        let at = base + site.at;
        let mut old = 0;
        // SAFETY: two bytes of the image, made writable for this and put back.
        unsafe {
            if VirtualProtect(at as *const _, NOP2.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
                return Err(site.at);
            }
            std::ptr::copy_nonoverlapping(NOP2.as_ptr(), at as *mut u8, NOP2.len());
            VirtualProtect(at as *const _, NOP2.len(), old, &mut old);
            FlushInstructionCache(GetCurrentProcess(), at as *const _, NOP2.len());
        }
    }
    Ok(())
}

/// Whether we run under Wine (its ntdll exports `wine_get_version`).
#[cfg(windows)]
fn on_wine() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    // SAFETY: plain lookups in ntdll, always loaded.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        !ntdll.is_null() && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch() {
        assert_eq!(parse_switch(None), Ok(None));
        assert_eq!(parse_switch(Some(" ")), Ok(None));
        assert_eq!(parse_switch(Some("1")), Ok(Some(true)));
        assert_eq!(parse_switch(Some("On")), Ok(Some(true)));
        assert_eq!(parse_switch(Some("0")), Ok(Some(false)));
        assert_eq!(parse_switch(Some(" off ")), Ok(Some(false)));
        assert!(parse_switch(Some("2")).is_err());
        assert!(parse_switch(Some("yes")).is_err());
        // Off by default under Wine, on elsewhere; the switch wins either way.
        assert!(!keep_watchdog(None, true));
        assert!(keep_watchdog(None, false));
        assert!(keep_watchdog(Some(true), true));
        assert!(!keep_watchdog(Some(false), false));
    }

    #[test]
    fn the_sites_are_the_jumps_to_the_crash() {
        let offsets: Vec<usize> = SITES.iter().map(|s| s.at).collect();
        assert_eq!(offsets, [0x2215bdc, 0x2215c70, 0x2215d84]);
        // jae rel8 to +0x2215c0c; je rel8 over the 2-byte mov / jmp that skips the crash.
        assert_eq!(SITES[0].at + 2 + usize::from(SITES[0].bytes[1]), 0x2215c0c);
        assert_eq!(SITES[0].bytes[0], 0x73);
        for site in &SITES[1..] {
            assert_eq!(site.bytes, [0x74, 0x04]);
        }
        assert_eq!(NOP2, [0x66, 0x90]);
        assert_eq!(BUILD.len(), 64);
    }

    /// An image with the three sites' bytes, `len` bytes long.
    fn image(len: usize) -> Vec<u8> {
        let mut image = vec![0xcc; len];
        for site in &SITES {
            image[site.at..site.at + 2].copy_from_slice(&site.bytes);
        }
        image
    }

    #[test]
    fn only_that_build_with_all_three_sites() {
        let good = image(0x2215e00);
        assert_eq!(check(BUILD, &good, &SITES), Ok(()));
        assert_eq!(check(&BUILD.to_ascii_uppercase(), &good, &SITES), Ok(()));
        let other = "0".repeat(64);
        assert_eq!(check(&other, &good, &SITES), Err(Refused::OtherBuild));
        // One site already changed (or another build's code): nothing.
        let mut patched = good.clone();
        patched[0x2215c70..0x2215c72].copy_from_slice(&NOP2);
        assert_eq!(check(BUILD, &patched, &SITES), Err(Refused::Differs(0x2215c70)));
        let mut last = good.clone();
        last[0x2215d85] = 0x05;
        assert_eq!(check(BUILD, &last, &SITES), Err(Refused::Differs(0x2215d84)));
        // An image that ends inside the last site.
        assert_eq!(check(BUILD, &good[..0x2215d85], &SITES), Err(Refused::Outside(0x2215d84)));
        assert_eq!(check(BUILD, &[], &SITES), Err(Refused::Outside(0x2215bdc)));
    }

    #[cfg(windows)]
    #[test]
    fn writes_the_nops() {
        let mut place = image(0x2215e00);
        // SAFETY: our own buffer, its sites inside it; nothing runs it.
        unsafe { write(place.as_mut_ptr() as usize, &SITES) }.unwrap();
        for site in &SITES {
            assert_eq!(place[site.at..site.at + 2], NOP2);
        }
        assert_eq!(place[0x2215bdb], 0xcc);
    }
}
