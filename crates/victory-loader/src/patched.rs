//! The title's code bytes our load-time patches have rewritten, so that two
//! of them never write over each other's bytes.
//!
//! `write_sites.rs`, `descriptor_sites.rs` and `wide.rs` each turn title
//! instructions into jumps to code of ours. A patch that overlaps another's
//! bytes would copy or rewrite a jump as if it were the title's instruction.
//! Each one checks its range here before writing and notes it after; a range
//! already taken is logged and left alone by the patch that comes second.

use std::sync::Mutex;

/// Address ranges `[start, end)` written so far, by start; none overlap.
static TAKEN: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn overlaps_in(taken: &[(usize, usize)], start: usize, end: usize) -> bool {
    // The first range that ends after `start`: ranges are disjoint and sorted
    // by start, so also by end.
    let i = taken.partition_point(|&(_, e)| e <= start);
    taken.get(i).is_some_and(|&(s, _)| s < end)
}

fn insert_in(taken: &mut Vec<(usize, usize)>, start: usize, end: usize) -> bool {
    if start >= end {
        return true;
    }
    if overlaps_in(taken, start, end) {
        return false;
    }
    let i = taken.partition_point(|&(s, _)| s < start);
    taken.insert(i, (start, end));
    true
}

/// Whether some patch already rewrote any byte of `[start, start + len)`.
pub fn overlaps(start: usize, len: usize) -> bool {
    let taken = TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    overlaps_in(&taken, start, start.saturating_add(len))
}

/// Notes `[start, start + len)` as rewritten. `false`, and nothing noted, if
/// some of it already was.
pub fn take(start: usize, len: usize) -> bool {
    let mut taken = TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    insert_in(&mut taken, start, start.saturating_add(len))
}

/// Forgets the ranges that meet `[start, start + len)`: for a test whose
/// memory may be another, freed test's.
#[cfg(test)]
pub fn forget(start: usize, len: usize) {
    let end = start.saturating_add(len);
    let mut taken = TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    taken.retain(|&(s, e)| e <= start || s >= end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_that_touches_another_is_refused() {
        let mut t = Vec::new();
        assert!(insert_in(&mut t, 0x100, 0x109));
        assert!(insert_in(&mut t, 0x200, 0x205));
        // Adjacent on either side: free.
        assert!(insert_in(&mut t, 0x109, 0x10e));
        assert!(insert_in(&mut t, 0xfb, 0x100));
        // One byte in common, at either end, or inside, or around.
        for (s, e) in [
            (0x108, 0x110),
            (0x1fc, 0x201),
            (0x202, 0x203),
            (0x1f0, 0x210),
        ] {
            assert!(overlaps_in(&t, s, e), "{s:#x}..{e:#x}");
            assert!(!insert_in(&mut t, s, e), "{s:#x}..{e:#x}");
        }
        assert_eq!(
            t,
            [
                (0xfb, 0x100),
                (0x100, 0x109),
                (0x109, 0x10e),
                (0x200, 0x205)
            ]
        );
        assert!(!overlaps_in(&t, 0x10e, 0x200));
        // An empty range overlaps nothing and is not kept.
        assert!(insert_in(&mut t, 0x102, 0x102));
        assert_eq!(t.len(), 4);
    }
}
