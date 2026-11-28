//! Whole-wave ("literal") control flow for a console wave64 compute program on a host of
//! 32-lane waves (D3DMetal), where it runs as two host waves that exchange through LDS.
//!
//! The per-lane form refuses a program whose cross-lane code (a vote, a reduction, a lane or
//! first-lane read, a mask read as a number, LDS one console wave shares) stands where not
//! every lane runs: the halves would have to meet there at a group barrier, and a barrier in
//! flow some threads of the group skip hangs the GPU. In the literal form
//! ([`crate::structure::structure_literal`]) every per-lane branch is taken as the console
//! takes it, by a 64-lane vote through the exchange, so both halves of the group (a group of
//! exactly one console wave, 64 threads) follow the same blocks and EXEC guards each vector
//! instruction. Every barrier then stands in flow the whole group runs:
//! - every block is one every lane runs ([`Lower::literal_checks`]);
//! - a scalar branch tests values both halves compute alike: the per-host-wave shortcuts (a
//!   loop over each lane's own bits, a waterfall's lane-own descriptor) are not taken;
//! - loops leave together: every exit is a vote, and the iteration counts are the group's;
//! - no barrier is made inside guarded code ([`BARRIER_GUARDED`]).
//!
//! Ported from pragyaangaur/VIctory-macOS (GPL-3.0, same lineage): #435 (`split_literal`,
//! whole-wave control flow for 64-thread groups), #591 (while-shaped loops in lockstep, which
//! the literal form gives every loop) and #593 (a branch whose two targets are loop back
//! edges, [`Lower::two_back_edges`]).

use super::*;

/// A group barrier made inside code only some lanes run, in whole-wave flow.
pub(crate) const BARRIER_GUARDED: Why = "a group barrier under a lane guard (whole-wave flow)";
/// A block not every lane runs, in whole-wave flow (an indirect exit, say).
pub(crate) const NOT_WHOLE: Why = "whole-wave flow where not every lane runs a block";
/// A waterfall loop, whose descriptor reads are the lane's own: its halves would part.
pub(crate) const WATERFALL: Why = "a resource waterfall in whole-wave flow";

/// Whether the literal form is the one to try for `native`, refused per lane on a host whose
/// waves are `host` lanes: a console wave64 compute program of exactly one console wave per
/// group (64 threads; with several, the waves of a group would decide apart and a barrier
/// would wait for threads that never come) on a host of 32-lane waves.
/// `VICTORY_WAVE_LITERAL=0` turns it off: such programs are then refused (the wave-wide
/// branches it replaced, `widen`, do not come back; part of the shader cache key).
pub(crate) fn applies(native: &NativeProgram, host: u32) -> bool {
    let on = std::env::var_os("VICTORY_WAVE_LITERAL").is_none_or(|v| v != "0");
    on && host == 32
        && native.stage == HwStage::Cs
        && native.wave64
        && matches!(&native.io, StageIo::Cs(cs) if cs.threads.iter().product::<u32>() == 64)
}

#[cfg(test)]
thread_local! {
    /// Tests: every guarded region opens with a group barrier, a fault the lowering does not
    /// make itself, to reach the refusal at the end of lowering ([`BARRIER_GUARDED`]).
    pub(crate) static INJECT_GUARDED_BARRIER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Lower<'_> {
    /// In whole-wave flow, before any block: the two host waves exchange, every block is one
    /// every lane runs, and no waterfall reads a lane's own descriptor.
    pub(super) fn literal_checks(&self) -> Result<(), Why> {
        if !self.x.st.literal {
            return Ok(());
        }
        if !self.x.res.waterfall.reads.is_empty() {
            return Err(WATERFALL);
        }
        if self.local {
            return Err(NOT_WHOLE);
        }
        match &self.exchange {
            // (a program the halves need not exchange in: nothing crosses them)
            None => Ok(()),
            Some((_, _, all)) if all.len() == self.x.p.blocks.len() && all.iter().all(|&a| a) => Ok(()),
            Some(_) => Err(NOT_WHOLE),
        }
    }

    /// A group barrier is being made at the current block: in whole-wave flow, never inside
    /// guarded code.
    pub(super) fn note_barrier(&mut self) {
        #[cfg(test)]
        self.barriers.push((self.cur_ir, self.guarded_depth > 0));
        if self.x.st.literal && self.guarded_depth > 0 {
            self.barrier_guarded = true;
        }
    }

    /// The terminator of IR block `b`'s branch on `cond` whose both targets, `taken` and
    /// `not_taken`, are loop headers it jumps back to (two nested loops whose latches share a
    /// block). A single back edge leaves its loop at [`LOOP_LIMIT`] by the branch's other,
    /// forward side; here both sides go back, so each gets a block of its own that counts its
    /// loop's iteration and, at the limit, leaves the program as an unconditional back edge
    /// does ([`Lower::abort_block`]). Below the limit both go where the console goes. In
    /// whole-wave flow every lane takes the same side with the same counts, so the halves
    /// reach the limit together. (pragyaangaur/VIctory-macOS #593, `lower/back_edges.rs`.)
    pub(super) fn two_back_edges(&mut self, b: usize, cond: V, taken: usize, not_taken: usize) -> Term {
        let side = |l: &mut Self, to: usize| {
            let ok = l.count_iteration(to, pred::IULT);
            let check = l.s.m.new_block();
            // the count goes to the header from `check`, and so do the header's phis
            if let Some(last) = l.counters.get_mut(&to).and_then(|c| c.1.last_mut()) {
                last.1 = check;
            }
            l.edges.insert((b, to), check);
            let abort = l.abort_block();
            let header = l.block[to].expect("reachable");
            l.s.m.terminate(check, Term::CondBr(ok, header, abort));
            check
        };
        let t = side(self, taken);
        let f = side(self, not_taken);
        Term::CondBr(cond, t, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{io, ir, resource, structure};

    /// [`super::super::super::literal_tests`]'s first-lane read in a lane branch, in
    /// whole-wave flow: its program, structure, resources and inputs.
    fn whole_wave() -> (NativeProgram, Program, Structured, Resources, Io) {
        let words = [
            0xD765_0003,
            0x0201_00C1, // v_mbcnt_lo_u32_b32 v3, -1, 0
            0xD766_0003,
            0x0202_06C1, // v_mbcnt_hi_u32_b32 v3, -1, v3
            0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
            0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0004, // s_cbranch_execz skip
            0x7E0C_0500, // v_readfirstlane_b32 s6, v0
            0x7E02_0206, // v_mov_b32 v1, s6
            0xE070_0000,
            0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
            0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
        ];
        let native = super::super::super::literal_tests::native(&words, 64);
        let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
        let st = structure::structure_literal(&p).expect("structure_literal");
        let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resources");
        let io = io::raise(&p, &native).expect("io");
        (native, p, st, res, io)
    }

    /// `check` on a [`Lower`] of [`whole_wave`] (made on a 32-lane host), with `res` changed by
    /// `edit` first.
    fn with_lower<R>(edit: impl FnOnce(&mut Resources), check: impl FnOnce(&mut Lower) -> R) -> R {
        let (native, p, st, mut res, io) = whole_wave();
        edit(&mut res);
        let parts = Parts {
            native: &native,
            p: &p,
            st: &st,
            io: &io,
            res: &res,
            inputs: Vec::new(),
            outputs: Vec::new(),
            prims: Vec::new(),
            mesh_stride: 0,
        };
        let bad = HashMap::new();
        let was = THREAD_MAX_WAVE.replace(Some(32));
        let r = check(&mut Lower::new(&parts, &bad));
        THREAD_MAX_WAVE.set(was);
        r
    }

    #[test]
    fn a_barrier_inside_guarded_code_is_refused_and_one_outside_is_not() {
        with_lower(
            |_| {},
            |l| {
                assert!(l.x.st.literal && l.exchange.is_some());
                assert_eq!(l.literal_checks(), Ok(()));
                let e = l.s.entry;
                l.group_sync(e);
                assert!(!l.barrier_guarded, "a barrier every lane gets to");
                let g = l.s.m.bool(true);
                let at = l.begin_guarded(Some(g));
                let c = l.cur;
                l.group_sync(c);
                l.end_guarded(at, &[]);
                assert!(l.barrier_guarded, "a barrier under a lane guard");
                assert_eq!(l.guarded_depth, 0);
            },
        );
    }

    #[test]
    fn a_waterfall_is_refused_in_whole_wave_flow() {
        with_lower(
            |res| res.waterfall.reads.push(Value(u32::MAX)),
            |l| assert_eq!(l.literal_checks(), Err(WATERFALL)),
        );
    }
}
