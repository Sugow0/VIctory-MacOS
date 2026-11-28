//! Where the group barriers of a console wave64 split into two host waves of 32 may go for
//! its LDS accesses ([`super::lds_meetings`]): the blocks every thread of the group runs
//! together, the same number of times ([`in_step`]). A group barrier some thread never
//! reaches, or reaches a different number of times, hangs the group, so these are stricter
//! than the blocks every lane runs ([`super::all_lane_blocks`]): a branch on a scalar the host
//! waves may not compute alike (a first lane's value, a lane mask, the wave's own id), and a
//! loop whose exit is per lane, keep their blocks out.
//!
//! Ported from pragyaangaur/VIctory-macOS PR #370 (`in_step` and the latch fix of
//! `branch_regions`, `lower/barriers.rs` and `lower/halves.rs`; GPL-3.0, same lineage).
//! There it placed the barriers of a one-wave group; here it chooses the blocks the split-LDS
//! lowering meets in, for groups of one console wave or several. Its other part, which lets a
//! write follow a read in code only some lanes run without a barrier ([`read_after_write`]),
//! is taken only with `VICTORY_LDS_WAR=1`: the two halves may then read what the other
//! already overwrote, which the console never does.

use super::{Atom, EXEC, Fields, HashSet, Inst, LaneExit, Program, Reg, Structured, Value};
use crate::ir::VCC;

/// Every branch (by its block) but those the wave takes together ([`Structured::wide`]), with
/// its region: the blocks reachable from it that do not post-dominate it, which only the lanes
/// (or host waves) taking one side may run. The branch's own block is in its region when a
/// cycle leads back to it (a loop's latch: only the lanes going round run it again); a block
/// counts as its own post-dominator, so [`super::lane_regions`] leaves it out unless asked (`loops`). Uniform
/// branches too, which [`in_step`] judges by their condition. None with an indirect exit.
fn branch_regions(st: &Structured) -> Option<Vec<(usize, Vec<bool>)>> {
    let n = st.exits.len();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, e) in st.exits.iter().enumerate() {
        match *e {
            LaneExit::Jump(t) => succ[b].push(t),
            LaneExit::Branch { taken, not_taken, .. } => {
                succ[b].push(taken);
                succ[b].push(not_taken);
            }
            LaneExit::End => {}
            LaneExit::Indirect => return None,
        }
    }
    // post-dominators (bitsets; a virtual exit after every End)
    let words = n.div_ceil(64);
    let full = vec![u64::MAX; words];
    let mut pdom: Vec<Vec<u64>> = (0..n)
        .map(|b| {
            if succ[b].is_empty() {
                let mut v = vec![0; words];
                v[b / 64] |= 1 << (b % 64);
                v
            } else {
                full.clone()
            }
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            if succ[b].is_empty() {
                continue;
            }
            let mut v = full.clone();
            for &t in &succ[b] {
                for w in 0..words {
                    v[w] &= pdom[t][w];
                }
            }
            v[b / 64] |= 1 << (b % 64);
            if v != pdom[b] {
                pdom[b] = v;
                changed = true;
            }
        }
    }
    let mut regions = Vec::new();
    for (d, e) in st.exits.iter().enumerate() {
        if !matches!(e, LaneExit::Branch { .. }) || st.wide.contains(&d) {
            continue;
        }
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = succ[d].clone();
        while let Some(b) = stack.pop() {
            if seen[b] || b != d && pdom[d][b / 64] >> (b % 64) & 1 == 1 {
                continue;
            }
            seen[b] = true;
            stack.extend(succ[b].iter().copied());
        }
        regions.push((d, seen));
    }
    Some(regions)
}

/// Per IR block, whether every thread of the group runs it together, the same number of
/// times, when each console wave runs as two host waves: outside the region of every branch
/// whose condition may differ between host waves ([`branch_regions`]). That is a per-lane
/// branch, or one on a scalar the host waves may not compute alike.
///
/// Alike are scalars from the group's SGPR inputs (not `info`, the thread-group info SGPR,
/// which holds the wave's id), constants and scalar loads without `glc`, through scalar code;
/// and first-lane reads of a VGPR every lane got from an alike scalar (`v_mov_b32`,
/// unguarded, both in step). Not hardware state, scalar atomics or coherent loads; without
/// `exact`, not lane masks (EXEC, VCC) nor other VALU results either.
///
/// `exact`: a group of one console wave whose halves exchange, where the translation gives
/// every scalar the console's value (lane masks, first-lane and lane reads go through the
/// exchange, or are refused where not every lane runs), except the values it names (those it
/// takes per host wave: a waterfall loop's reads, a loop over a lane's own bits).
///
/// Scalars start alike (a loop counter needs itself) and are narrowed together with the
/// regions to a fixed point. None with an indirect exit.
pub(super) fn in_step(
    p: &Program,
    st: &Structured,
    info: Option<u8>,
    exact: Option<&HashSet<Value>>,
) -> Option<Vec<bool>> {
    let regions = branch_regions(st)?;
    let n = st.exits.len();
    let mask = |r: Reg| matches!(r, Reg::S(k) if [EXEC, EXEC + 1, VCC, VCC + 1].contains(&k));
    let scalar = |r: Reg| matches!(r, Reg::S(_) | Reg::Scc) && (exact.is_some() || !mask(r));
    let guarded = |b: usize, i: usize| st.guards.get(b).and_then(|g| g.get(i)).copied().flatten().is_some();
    // Scalar writes no two host waves need compute alike: hardware state, wave ids, scalar
    // atomics and stores, loads the group may write (`glc`), vector code (unless `exact`).
    let own = |inst: &Inst| {
        let name = inst.name();
        let per_wave = |inst: &Inst| exact.is_none_or(|e| inst.writes.iter().any(|w| e.contains(&w.1)));
        match &inst.fields {
            Fields::Sop { .. } => {
                name.starts_with("s_getreg")
                    || name.starts_with("s_sendmsg")
                    || name.contains("waveid")
                    || exact.is_some_and(|e| inst.writes.iter().any(|w| e.contains(&w.1)))
            }
            Fields::Smem { cache, .. } => {
                !(name.starts_with("s_load") || name.starts_with("s_buffer_load")) || cache.glc
            }
            _ => per_wave(inst),
        }
    };
    let inputs = || {
        p.inputs
            .iter()
            .filter(|(r, _)| scalar(*r) && !matches!((*r, info), (Reg::S(k), Some(i)) if k == i))
            .map(|(_, v)| *v)
    };
    let mut alike: HashSet<Value> = inputs()
        .chain(p.blocks.iter().flat_map(|b| {
            b.phis.iter().filter(|f| scalar(f.reg)).map(|f| f.value).chain(
                b.insts.iter().flat_map(|i| i.writes.iter().filter(|w| scalar(w.0)).map(|w| w.1)),
            )
        }))
        .collect();
    loop {
        let mut step = vec![true; n];
        for (d, region) in &regions {
            let LaneExit::Branch { when, .. } = st.exits[*d] else {
                continue;
            };
            let together =
                st.bools.support(when).iter().all(|a| matches!(a, Atom::Uniform(v) if alike.contains(v)));
            if !together {
                for (b, &r) in region.iter().enumerate() {
                    step[b] &= !r;
                }
            }
        }
        // VGPRs every lane holds alike: an unguarded copy of an alike scalar or a constant
        let mut vgprs: HashSet<Value> = HashSet::new();
        for (b, block) in p.blocks.iter().enumerate().take(n).filter(|&(b, _)| step[b]) {
            for (i, inst) in block.insts.iter().enumerate() {
                if inst.name() == "v_mov_b32"
                    && !guarded(b, i)
                    // (the old value of the destination too, which no lane keeps)
                    && inst.reads.iter().all(|&(r, v)| {
                        mask(r) || scalar(r) && alike.contains(&v) || inst.writes.iter().any(|w| w.0 == r)
                    })
                {
                    vgprs.extend(inst.writes.iter().filter(|w| matches!(w.0, Reg::V(_))).map(|w| w.1));
                }
            }
        }
        // (only shrinks with `alike`, so this ends)
        let mut next: HashSet<Value> = inputs().collect();
        for (b, block) in p.blocks.iter().enumerate().take(n) {
            // every host wave enters by the same edge
            let entered = step[b] && block.preds.iter().all(|&q| step.get(q).copied().unwrap_or(false));
            for phi in &block.phis {
                if scalar(phi.reg) && entered && phi.args.iter().all(|a| alike.contains(a)) {
                    next.insert(phi.value);
                }
            }
            for (i, inst) in block.insts.iter().enumerate() {
                // a first-lane read of a VGPR every lane holds alike
                let first = inst.name() == "v_readfirstlane_b32"
                    && step[b]
                    && !guarded(b, i)
                    && inst.reads.iter().all(|&(r, v)| mask(r) || vgprs.contains(&v));
                let from = inst
                    .reads
                    .iter()
                    .all(|&(r, v)| scalar(r) && alike.contains(&v) || exact.is_some() && matches!(r, Reg::V(_)));
                if from && !own(inst) || first {
                    next.extend(inst.writes.iter().filter(|w| scalar(w.0)).map(|w| w.1));
                }
            }
        }
        if next.len() == alike.len() {
            return Some(step);
        }
        alike = next;
    }
}

/// Whether an LDS read in `blocks` (a region of [`super::lds_meetings`]: blocks only some
/// lanes run, joined by their edges) may come after a write in it, as the console runs the
/// wave: later in the code (the console runs a region's blocks in their order, every lane of
/// the wave at once, so the lanes of one side of a branch read what those of the other side
/// wrote before), or again round a cycle.
///
/// A region whose reads all come first is taken without a barrier inside (none can stand
/// there): each host wave reads before it writes, as the console does. shortcut: the other
/// host wave may already have written what a read gets, which is wrong only where the two
/// halves of a console wave read and write the same dword in the region (pragyaangaur/
/// VIctory-macOS PR #370 takes such a write after a read the same way).
pub(super) fn read_after_write(p: &Program, st: &Structured, blocks: &[usize]) -> bool {
    let inside: HashSet<usize> = blocks.iter().copied().collect();
    // (reads, writes) of an LDS access
    let access = |i: &Inst| {
        let name = i.name();
        if !matches!(i.fields, Fields::Ds { .. }) || name.starts_with("ds_swizzle") || name.contains("permute") {
            return None;
        }
        // (an atomic, even one that returns the old value, as a write: lanes take turns at
        // it in no order the program can rely on, as in [`super::lds_meetings`])
        Some((name.starts_with("ds_read"), !name.starts_with("ds_read")))
    };
    let succ = |b: usize| -> Vec<usize> {
        match st.exits[b] {
            LaneExit::Jump(t) => vec![t],
            LaneExit::Branch { taken, not_taken, .. } => vec![taken, not_taken],
            LaneExit::End | LaneExit::Indirect => Vec::new(),
        }
    };
    let mut first_write: Option<usize> = None;
    let mut last_read: Option<usize> = None;
    let mut write_blocks = Vec::new();
    let mut read_blocks = HashSet::new();
    for &b in blocks {
        for i in &p.blocks[b].insts {
            let Some((r, w)) = access(i) else { continue };
            let at = i.machine.offset;
            if w {
                first_write = Some(first_write.map_or(at, |f| f.min(at)));
                write_blocks.push(b);
            }
            if r {
                last_read = Some(last_read.map_or(at, |l| l.max(at)));
                read_blocks.insert(b);
            }
        }
    }
    let (Some(w), Some(r)) = (first_write, last_read) else {
        return false;
    };
    if w < r {
        return true;
    }
    // a read reached from a write without leaving the region (round a loop)
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack: Vec<usize> = write_blocks.iter().flat_map(|&b| succ(b)).collect();
    while let Some(b) = stack.pop() {
        if !inside.contains(&b) || !seen.insert(b) {
            continue;
        }
        if read_blocks.contains(&b) {
            return true;
        }
        stack.extend(succ(b));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::{Parts, THREAD_MAX_WAVE, all_lane_blocks, lds_meetings, lds_meetings_with, lower};
    use crate::native::{ComputeIo, HwStage, NativeProgram, RootParam, RootType, StageIo};
    use crate::{io, ir, resource, structure};

    const S_ENDPGM: u32 = 0xBF81_0000;
    const V_LSHLREV_V1_2_V0: u32 = 0x3402_0082; // v_lshlrev_b32 v1, 2, v0
    const DS_WRITE_V1_V0: [u32; 2] = [0xD834_0000, 0x0000_0001]; // ds_write_b32 v1, v0
    const DS_READ_V2_V1_4: [u32; 2] = [0xD8D8_0004, 0x0200_0001]; // ds_read_b32 v2, v1 offset:4
    const S_MOV_S20_0: u32 = 0xBE94_0380; // s_mov_b32 s20, 0
    const S_ADD_S20_1: u32 = 0x8014_8114; // s_add_u32 s20, s20, 1
    const S_CMP_LT_S20_4: u32 = 0xBF0A_8414; // s_cmp_lt_u32 s20, 4
    const S_CMP_LT_S20_S21: u32 = 0xBF0A_1514; // s_cmp_lt_u32 s20, s21
    const S_CMP_LT_S20_S2: u32 = 0xBF0A_0214; // s_cmp_lt_u32 s20, s2
    const READFIRSTLANE_S21_V0: u32 = 0x7E2A_0500; // v_readfirstlane_b32 s21, v0
    const READFIRSTLANE_S21_V3: u32 = 0x7E2A_0503; // v_readfirstlane_b32 s21, v3
    const V_MOV_V3_S0: u32 = 0x7E06_0200; // v_mov_b32 v3, s0
    const V_CMP_GT_VCC_16_V0: u32 = 0x7D88_0090; // v_cmp_gt_u32 vcc, 16, v0
    const SAVE_EXEC: u32 = 0xBE8C_246A; // s_and_saveexec_b64 s[12:13], vcc
    const RESTORE_EXEC: u32 = 0xBEFE_040C; // s_mov_b64 exec, s[12:13]
    /// `v_mbcnt_lo_u32_b32 v5, -1, 0`: code that asks for the console's lanes (the halves
    /// then exchange, and a group of several console waves is split too).
    const MBCNT: [u32; 2] = [0xD765_0005, 0x0001_00C1];
    /// Two user SGPRs (s0, s1), 512 bytes of LDS.
    const RSRC2: u32 = 1 << 15 | 2 << 1;
    /// [`RSRC2`] with the thread-group info SGPR (s2, the wave's id in the group).
    const RSRC2_INFO: u32 = RSRC2 | 1 << 10;

    /// The LDS write, then the read of another lane's dword.
    fn write_read() -> Vec<u32> {
        [DS_WRITE_V1_V0, DS_READ_V2_V1_4].concat()
    }

    /// `body` in a loop on s20, ending at the compare `cmp` (`s_cbranch_scc1` back to the
    /// body's start), after `before`.
    fn looped(before: &[u32], body: &[u32], cmp: u32) -> Vec<u32> {
        let mut w = before.to_vec();
        w.push(S_MOV_S20_0);
        w.extend(body);
        let back = -(body.len() as i32 + 3);
        w.extend([S_ADD_S20_1, cmp, 0xBF85_0000 | (back as u32 & 0xFFFF)]);
        w.push(S_ENDPGM);
        w
    }

    /// The write and read in a loop whose exit is per lane (`s_cbranch_vccnz` back).
    fn lane_loop() -> Vec<u32> {
        let mut w = vec![V_LSHLREV_V1_2_V0];
        w.extend(write_read());
        w.extend([
            0x7D88_0480, // v_cmp_gt_u32 vcc, 0, v2
            0xBF87_FFFA, // s_cbranch_vccnz (back to the write)
            S_ENDPGM,
        ]);
        w
    }

    /// `if (v0 < 16) return;` around the write and the read: the lanes below 16 leave for
    /// the end (`s_cbranch_execz` to a block that only ends the program).
    fn early_exit() -> Vec<u32> {
        let mut w = vec![V_LSHLREV_V1_2_V0, 0x7D82_0090]; // v_cmp_lt_u32 vcc, 16, v0
        w.extend([SAVE_EXEC, 0xBF88_0000 | (write_read().len() as u32 + 1)]);
        w.extend(write_read());
        w.push(S_ENDPGM);
        w.push(S_ENDPGM);
        w
    }

    fn native(words: &[u32], threads: u32, rsrc2: u32) -> NativeProgram {
        NativeProgram {
            stage: HwStage::Cs,
            wave64: true,
            vgprs: 16,
            // s0, s1: root constants
            layout: vec![RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 2,
                base_register: 0,
                user_data: 0,
                slot: 0,
            }],
            used: vec![],
            io: StageIo::Cs(ComputeIo { rsrc2, threads: [threads, 1, 1] }),
            code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        }
    }

    fn build(words: &[u32]) -> ir::Program {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        ir::build(&code, true, 16).unwrap()
    }

    /// The wave size `p` (the program of `native`) lowers to on a host of 32-lane waves: 32
    /// when its console waves run as two host waves, the LDS met in step.
    fn wave_of(native: &NativeProgram, p: &ir::Program) -> Option<u32> {
        let st = structure::structure(p).unwrap();
        let res = resource::track(p, &native.layout, native.stage, Some(&st)).unwrap();
        let io = io::raise(p, native).unwrap();
        let was = THREAD_MAX_WAVE.replace(Some(32));
        let l = lower(&Parts {
            native,
            p,
            st: &st,
            io: &io,
            res: &res,
            inputs: vec![],
            outputs: vec![],
            prims: vec![],
            mesh_stride: 0,
        });
        THREAD_MAX_WAVE.set(was);
        let l = l.expect("lowers");
        // a group of several console waves declares no wave size: in halves unless 64
        l.wave_size.or(Some(32))
    }

    fn wave(words: &[u32], threads: u32, rsrc2: u32) -> Option<u32> {
        wave_of(&native(words, threads, rsrc2), &build(words))
    }

    /// [`super::in_step`] with no info SGPR, no exchanged first-lane read.
    fn step(p: &ir::Program) -> Vec<bool> {
        let st = structure::structure(p).unwrap();
        super::in_step(p, &st, None, None).unwrap()
    }

    #[test]
    fn the_programs_are_what_their_comments_say() {
        let text = |w: &[u32]| {
            let code: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
            crate::gfx10::decode(&code).unwrap_or_else(|e| panic!("{e:?} {w:08x?}")).iter().map(|i| i.text(true)).collect::<Vec<_>>().join("\n")
        };
        let all = [
            text(&looped(&[V_LSHLREV_V1_2_V0, READFIRSTLANE_S21_V0, V_MOV_V3_S0, READFIRSTLANE_S21_V3], &write_read(), S_CMP_LT_S20_S21)),
            text(&looped(&[], &[], S_CMP_LT_S20_4)),
            text(&looped(&[], &[], S_CMP_LT_S20_S2)),
            text(&lane_loop()),
            text(&early_exit()),
            text(&[V_CMP_GT_VCC_16_V0, SAVE_EXEC, RESTORE_EXEC, S_ENDPGM]),
        ]
        .join("\n");
        for want in [
            "v_lshlrev_b32 v1, 2, v0",
            "v_readfirstlane_b32 s21, v0",
            "v_readfirstlane_b32 s21, v3",
            "v_mov_b32 v3, s0",
            "s_mov_b32 s20, 0",
            "ds_write_b32 v1, v0",
            "ds_read_b32 v2, v1 offset:4",
            "s_add_u32 s20, s20, 1",
            "s_cmp_lt_u32 s20, s21",
            "s_cmp_lt_u32 s20, 4",
            "s_cmp_lt_u32 s20, s2",
            "s_cbranch_scc1",
            "v_cmp_gt_u32 vcc, 0, v2",
            "s_cbranch_vccnz",
            "v_cmp_lt_u32 vcc, 16, v0",
            "v_cmp_gt_u32 vcc, 16, v0",
            "s_and_saveexec_b64 s[12:13], vcc",
            "s_cbranch_execz",
            "s_mov_b64 exec, s[12:13]",
        ] {
            assert!(all.contains(want), "{want:?} not in:\n{all}");
        }
    }

    /// `before` with [`MBCNT`] in front when `lanes`.
    fn with_lanes(lanes: bool, before: &[u32]) -> Vec<u32> {
        let mut w = if lanes { MBCNT.to_vec() } else { Vec::new() };
        w.extend(before);
        w
    }

    #[test]
    fn lds_in_a_loop_on_a_scalar_count_meets_inside_the_loop() {
        // A count every host wave computes alike: the loop's blocks are in step, and the
        // console waves run in halves with barriers in the loop (as before).
        let w = looped(&[V_LSHLREV_V1_2_V0], &write_read(), S_CMP_LT_S20_4);
        let p = build(&w);
        assert!(p.blocks[1].insts[0].name().starts_with("ds_write"));
        assert_eq!(step(&p), [true, true, true]);
        assert_eq!(wave(&w, 64, RSRC2), Some(32));
        let w = looped(&with_lanes(true, &[V_LSHLREV_V1_2_V0]), &write_read(), S_CMP_LT_S20_4);
        for threads in [64, 128] {
            assert_eq!(wave(&w, threads, RSRC2), Some(32), "{threads} threads");
        }
        // So is a count read from the first lane of a copy of a root constant (`s0`).
        let w = looped(&[V_LSHLREV_V1_2_V0, V_MOV_V3_S0, READFIRSTLANE_S21_V3], &write_read(), S_CMP_LT_S20_S21);
        assert_eq!(step(&build(&w)), [true, true, true]);
        assert_eq!(wave(&w, 64, RSRC2), Some(32));
    }

    #[test]
    fn lds_in_a_loop_on_a_lane_count_keeps_its_barriers_out() {
        // The count is the first lane's thread id: 0 in the lower host wave, 32 in the upper.
        // A barrier in the loop would have the upper wave wait 31 times for nobody (it got
        // them before this port): the console wave keeps 64 lanes.
        let w = looped(&[V_LSHLREV_V1_2_V0, READFIRSTLANE_S21_V0], &write_read(), S_CMP_LT_S20_S21);
        let p = build(&w);
        assert_eq!(all_lane_blocks(&structure::structure(&p).unwrap()).unwrap(), [true, true, true]);
        assert_eq!(step(&p), [true, false, true]);
        assert_eq!(wave(&w, 64, RSRC2), Some(64));
        // Halves that exchange read the console's first lane, alike in both: in step. Not
        // in a group of two console waves, whose first lanes differ.
        let w = looped(&with_lanes(true, &[V_LSHLREV_V1_2_V0, READFIRSTLANE_S21_V0]), &write_read(), S_CMP_LT_S20_S21);
        assert_eq!(wave(&w, 64, RSRC2), Some(32));
        assert_eq!(wave(&w, 128, RSRC2), Some(64));
    }

    #[test]
    fn lds_in_a_loop_on_the_waves_id_keeps_its_barriers_out() {
        // The count is the thread-group info SGPR (s2): one per console wave, which the
        // waves of a group of two do not share. (The lowering refuses that SGPR as yet.)
        let w = looped(&[V_LSHLREV_V1_2_V0], &write_read(), S_CMP_LT_S20_S2);
        let p = build(&w);
        let st = structure::structure(&p).unwrap();
        assert_eq!(super::in_step(&p, &st, None, None).unwrap(), [true, true, true], "s2 a plain input");
        assert_eq!(super::in_step(&p, &st, Some(2), None).unwrap(), [true, false, true]);
    }

    #[test]
    fn lds_in_a_loop_whose_exit_is_per_lane_keeps_its_barriers_out() {
        // The latch is the branch's own block: only the lanes going round run it again.
        for lanes in [false, true] {
            let w = with_lanes(lanes, &lane_loop());
            let p = build(&w);
            let st = structure::structure(&p).unwrap();
            let b = p.blocks.iter().position(|b| b.insts.iter().any(|i| i.name().starts_with("ds_"))).unwrap();
            assert!(all_lane_blocks(&st).unwrap()[b], "lane_regions leaves the latch out");
            assert!(!step(&p)[b]);
            // a read of a write in the loop, which only some lanes run: no barrier orders them
            assert!(lds_meetings(&p, &st, &step(&p)).is_none());
            assert_eq!(wave(&w, 64, RSRC2), Some(64));
            if lanes {
                assert_eq!(wave(&w, 128, RSRC2), Some(64));
            }
        }
    }

    /// `if (v0 < 16) { body }` (`s_and_saveexec`, `s_cbranch_execz` past it, EXEC restored).
    fn lane_if(body: &[u32]) -> Vec<u32> {
        let mut w = vec![V_LSHLREV_V1_2_V0, V_CMP_GT_VCC_16_V0, SAVE_EXEC, 0xBF88_0000 | body.len() as u32];
        w.extend(body);
        w.extend([RESTORE_EXEC, S_ENDPGM]);
        w
    }

    #[test]
    fn lds_read_then_written_in_a_lane_branch_runs_in_halves_when_asked() {
        // Reads first, then the write: each host wave reads before it writes, as the
        // console's wave does. With the shortcut (`VICTORY_LDS_WAR=1`) the region is met at
        // its edges only; without it (the default) refused, as before.
        let w = lane_if(&[DS_READ_V2_V1_4, DS_WRITE_V1_V0].concat());
        let p = build(&w);
        let st = structure::structure(&p).unwrap();
        let step = step(&p);
        let meet = lds_meetings_with(&p, &st, &step, true).expect("met");
        for (b, &(start, end)) in meet.iter().enumerate() {
            assert!(!(start || end) || step[b], "a meeting in block {b}, which not every lane runs");
        }
        assert!(meet.iter().any(|m| m.0 || m.1));
        assert!(lds_meetings_with(&p, &st, &step, false).is_none());
        // The write first: refused even with the shortcut.
        let q = build(&lane_if(&write_read()));
        let sq = structure::structure(&q).unwrap();
        assert!(lds_meetings_with(&q, &sq, &super::in_step(&q, &sq, None, None).unwrap(), true).is_none());
    }

    #[test]
    fn lds_written_then_read_in_a_lane_branch_is_refused() {
        // A read of what the other host wave may not have written yet: no barrier can go
        // between them, so the console wave keeps 64 lanes (as before).
        let w = lane_if(&write_read());
        let p = build(&w);
        assert!(lds_meetings(&p, &structure::structure(&p).unwrap(), &step(&p)).is_none());
        assert_eq!(wave(&w, 64, RSRC2), Some(64));
    }

    #[test]
    fn lds_after_a_lane_branch_meets_at_its_edges() {
        // `if (v0 < 16) write;` then a read after the join: the waves meet before the
        // branch's region and at the join, blocks every lane runs together (as before).
        let mut w = vec![V_LSHLREV_V1_2_V0, V_CMP_GT_VCC_16_V0, SAVE_EXEC, 0xBF88_0002];
        w.extend(DS_WRITE_V1_V0);
        w.push(RESTORE_EXEC);
        w.extend(DS_READ_V2_V1_4);
        w.push(S_ENDPGM);
        let p = build(&w);
        let st = structure::structure(&p).unwrap();
        let step = step(&p);
        assert_eq!(step, all_lane_blocks(&st).unwrap());
        let meet = lds_meetings(&p, &st, &step).expect("met");
        for (b, &(start, end)) in meet.iter().enumerate() {
            assert!(!(start || end) || step[b], "a meeting in block {b}, which not every lane runs");
        }
        assert!(meet.iter().any(|m| m.1) && meet.iter().any(|m| m.0));
        assert_eq!(wave(&w, 64, RSRC2), Some(32));
    }

    #[test]
    fn lanes_that_exit_early_still_meet_the_barriers() {
        // The lanes below 16 leave for the end: the write and read are in a region only the
        // others run, refused as it is.
        let w = early_exit();
        let n = native(&w, 64, RSRC2);
        let p = build(&w);
        let st = structure::structure(&p).unwrap();
        assert!(!step(&p).iter().all(|&s| s));
        assert!(lds_meetings(&p, &st, &step(&p)).is_none());
        assert_eq!(wave_of(&n, &p), Some(64));
        // Kept on to the end with EXEC off (`ir::without_early_outs`, the form the refusal is
        // retried in; pragyaangaur/VIctory-macOS PR #511's `keep_lanes_to_end`), every lane
        // runs the blocks with the LDS accesses, so every one reaches their barriers.
        let q = ir::without_early_outs(&p).expect("an early-out");
        let step = step(&q);
        for (b, block) in q.blocks.iter().enumerate() {
            if block.insts.iter().any(|i| i.name().starts_with("ds_")) {
                assert!(step[b], "block {b}'s LDS accesses are not met by every lane");
            }
        }
        assert_eq!(wave_of(&n, &q), Some(32));
    }

    #[test]
    fn a_64_lane_host_is_unchanged() {
        // The lane-count loop on a host of 64-lane waves: one wave, no split, no barrier.
        let w = looped(&[V_LSHLREV_V1_2_V0, READFIRSTLANE_S21_V0], &write_read(), S_CMP_LT_S20_S21);
        let n = native(&w, 64, RSRC2);
        let p = build(&w);
        let st = structure::structure(&p).unwrap();
        let res = resource::track(&p, &n.layout, n.stage, Some(&st)).unwrap();
        let io = io::raise(&p, &n).unwrap();
        let was = THREAD_MAX_WAVE.replace(Some(64));
        let l = lower(&Parts {
            native: &n,
            p: &p,
            st: &st,
            io: &io,
            res: &res,
            inputs: vec![],
            outputs: vec![],
            prims: vec![],
            mesh_stride: 0,
        });
        THREAD_MAX_WAVE.set(was);
        assert_eq!(l.expect("lowers").wave_size, Some(64));
    }
}
