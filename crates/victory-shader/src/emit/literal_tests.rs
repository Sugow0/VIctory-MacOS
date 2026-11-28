//! Whole-wave ("literal") control flow for console wave64 compute programs on a host of
//! 32-lane waves ([`crate::structure::structure_literal`], `lower::literal`; after
//! pragyaangaur/VIctory-macOS #435, #591 and #593): each shape below is refused per lane, with
//! its cross-lane code where not every lane runs, and translates in whole-wave flow, as two
//! host waves of 32 that run every block together. Native (no host GPU): lowering only.

use super::lower;
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::structure::{Atom, LaneExit, Structured};
use crate::{ShaderError, io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

/// A wave64 compute program of `threads` threads (the thread index in v0) with root constants
/// in s0..s3 and a raw UAV's descriptor in s[8:11], then `body`.
pub(super) fn native(body: &[u32], threads: u32) -> NativeProgram {
    native_lds(body, threads, 0)
}

/// [`native`] with `lds_bytes` of LDS (a multiple of 512).
fn native_lds(body: &[u32], threads: u32, lds_bytes: u32) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1 | (lds_bytes / 512) << 15, // 5 user SGPRs: s0..s4; LDS_SIZE
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// Each waiting group barrier the lowering made, in order: the IR block, and whether it stood
/// inside guarded code.
type Barriers = Vec<(usize, bool)>;

/// `native` lowered with `st` on a host whose waves have at most `host` lanes: the wave size
/// it asks of the host, and its group barriers.
fn lower_with(
    native: &NativeProgram,
    p: &ir::Program,
    st: &Structured,
    host: u32,
) -> Result<(Option<u32>, Barriers), ShaderError> {
    let res = resource::track(p, &native.layout, native.stage, Some(st))?;
    let io = io::raise(p, native)?;
    let was = lower::THREAD_MAX_WAVE.replace(Some(host));
    lower::take_needs();
    let r = lower::lower_counted(
        &lower::Parts {
            native,
            p,
            st,
            io: &io,
            res: &res,
            inputs: Vec::new(),
            outputs: Vec::new(),
            prims: Vec::new(),
            mesh_stride: 0,
        },
        false,
    );
    lower::take_needs();
    lower::THREAD_MAX_WAVE.set(was);
    r.map(|l| (l.wave_size, l.barriers))
}

/// `body` (64 threads) is refused per lane on a 32-lane host where not every lane runs, and
/// translates for waves of 32 in whole-wave flow, whose every lane branch is a vote.
fn refused_then_literal(body: &[u32]) -> (ir::Program, Structured, Barriers) {
    refused_then_literal_of(&native(body, 64))
}

/// [`refused_then_literal`] for `native`. Both host waves meet at every group barrier made:
/// none stands in guarded code, they come in the blocks' order (each block once, as both
/// halves run it), and each lane branch's vote is an exchange (two barriers) in its block.
fn refused_then_literal_of(native: &NativeProgram) -> (ir::Program, Structured, Barriers) {
    let native = native.clone();
    assert!(lower::literal::applies(&native, 32));
    let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
    let per_lane = structure::structure(&p).expect("structure");
    assert!(!per_lane.literal);
    match lower_with(&native, &p, &per_lane, 32) {
        Err(ShaderError::Unsupported { what, .. }) => {
            assert!(lower::needs_every_lane(what), "per lane: {what}");
        }
        other => panic!("per lane, expected a refusal where not every lane runs: {other:?}"),
    }
    let st = structure::structure_literal(&p).expect("structure_literal");
    assert!(st.literal);
    every_lane_branch_votes(&st);
    let (wave, barriers) = lower_with(&native, &p, &st, 32).expect("whole-wave flow");
    assert_eq!(wave, Some(32), "two host waves of 32");
    assert!(barriers.iter().all(|&(_, guarded)| !guarded), "a barrier under a guard: {barriers:?}");
    let order = ir::reverse_postorder(&p.blocks);
    let rank = |b: usize| order.iter().position(|&x| x == b).expect("reachable");
    assert!(
        barriers.windows(2).all(|w| rank(w[0].0) <= rank(w[1].0)),
        "barriers out of the blocks' order: {barriers:?}"
    );
    for &b in &st.wide {
        let here = barriers.iter().filter(|&&(at, _)| at == b).count();
        assert!(here >= 2, "block {b}: its vote meets in {here} barriers: {barriers:?}");
    }
    (p, st, barriers)
}

/// Every branch on a lane's own bits is taken by the whole wave (with a known meaning), so no
/// block is one only some lanes run.
fn every_lane_branch_votes(st: &Structured) {
    for (b, e) in st.exits.iter().enumerate() {
        if let LaneExit::Branch { when, taken, not_taken } = *e
            && taken != not_taken
            && !st.bools.support(when).iter().all(|a| matches!(a, Atom::Uniform(_)))
        {
            assert!(st.wide.contains(&b), "block {b}: a lane branch not taken wave-wide");
            assert!(st.wave_any[b].is_some(), "block {b}: no whole-wave meaning");
        }
    }
}

/// Each lane's number in the console wave, stored: the program asks for the console's lanes
/// (a first-lane read alone is taken per host wave, as before).
const LANE_NUMBER: [u32; 6] = [
    0xD765_0003,
    0x0201_00C1, // v_mbcnt_lo_u32_b32 v3, -1, 0
    0xD766_0003,
    0x0202_06C1, // v_mbcnt_hi_u32_b32 v3, -1, v3
    0xE070_0004,
    0x8002_0300, // buffer_store_dword v3, off, s[8:11], 0 offset:4
];

/// [`LANE_NUMBER`], then `body`.
fn with_lanes(body: &[u32]) -> Vec<u32> {
    [&LANE_NUMBER[..], body].concat()
}

/// `if (t < s0) { s6 = readfirstlane(t); store }`: the console's first lane of the branch.
const FIRST_LANE_IN_BRANCH: [u32; 8] = [
    0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
    0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
    0xBF88_0004, // s_cbranch_execz skip
    0x7E0C_0500, // v_readfirstlane_b32 s6, v0
    0x7E02_0206, // v_mov_b32 v1, s6
    0xE070_0000,
    0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
    0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
];

#[test]
fn a_first_lane_read_in_a_lane_branch_translates_in_whole_wave_flow() {
    let (p, st, _) = refused_then_literal(&with_lanes(&FIRST_LANE_IN_BRANCH));
    // the branch's body keeps EXEC as a guard on what it does, as on the console
    let inside = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "v_readfirstlane_b32"))
        .expect("the body");
    assert!(st.guards[inside].iter().any(Option::is_some));
}

#[test]
fn a_lane_read_in_a_lane_branch_translates_in_whole_wave_flow() {
    refused_then_literal(&[
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0005, // s_cbranch_execz skip
        0xD760_0006,
        0x0200_0300, // v_readlane_b32 s6, v0, s1
        0x7E02_0206, // v_mov_b32 v1, s6
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
    ]);
}

#[test]
fn a_mask_counted_in_a_lane_branch_translates_in_whole_wave_flow() {
    refused_then_literal(&[
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0006, // s_cbranch_execz skip
        0x7D88_0001, // v_cmp_gt_u32 vcc, s1, v0
        0x878E_7E6A, // s_and_b64 s[14:15], vcc, exec
        0xBE86_100E, // s_bcnt1_i32_b64 s6, s[14:15]
        0x7E02_0206, // v_mov_b32 v1, s6
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
    ]);
}

#[test]
fn a_wave_reduction_in_a_lane_branch_translates_in_whole_wave_flow() {
    // thread t ORs `1 << (t >> 1)` over the threads below s0 (as `wave64_tests::program`)
    let mut body = vec![
        0x7E0A_0281, // v_mov_b32 v5, 1
        0x2C0C_0081, // v_lshrrev_b32 v6, 1, v0
        0x340E_0B06, // v_lshlrev_b32 v7, v6, v5
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0000 | 23, // s_cbranch_execz past the body
    ];
    body.extend_from_slice(&super::wave::WAVE_OR_V7_TO_S7);
    body.extend_from_slice(&[
        0x7E00_0207, // v_mov_b32 v0, s7
        0x38 << 26 | 0x1C << 18,
        0x80 << 24 | 2 << 16, // buffer_store_dword v0, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    ]);
    refused_then_literal(&body);
}

/// A while loop as the console compiler lays one out (#591): the entry jumps to the test,
/// which branches back into the body while lanes stay; the body reads the first lane.
const WHILE_FIRST_LANE: [u32; 13] = [
    0x7E02_0280, // v_mov_b32 v1, 0
    0x7E04_0300, // v_mov_b32 v2, v0
    0xBE8C_047E, // s_mov_b64 s[12:13], exec
    0xBF82_0003, // s_branch test
    0x7E0C_0502, // v_readfirstlane_b32 s6, v2 (body:)
    0x4A02_0206, // v_add_nc_u32 v1, s6, v1
    0x4A04_0481, // v_add_nc_u32 v2, 1, v2
    0x7D88_0400, // v_cmp_gt_u32 vcc, s0, v2 (test:)
    0x87FE_6A7E, // s_and_b64 exec, exec, vcc
    0xBF89_FFFA, // s_cbranch_execnz body
    0xBEFE_040C, // s_mov_b64 exec, s[12:13]
    0xE070_0000,
    0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
];

#[test]
fn a_while_loop_the_lanes_leave_one_by_one_runs_in_lockstep() {
    let (p, st, _) = refused_then_literal(&with_lanes(&WHILE_FIRST_LANE));
    // the loop's one exit, at the test, is the whole wave's: it stays while some lane does
    // (`execnz`), so both host waves run every pass together and leave together
    let test = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == "s_cbranch_execnz"))
        .expect("the test");
    assert!(st.wide.contains(&test));
    assert_eq!(st.wave_any[test], Some(true));
    let LaneExit::Branch { taken, .. } = st.exits[test] else { panic!("the test branches") };
    assert!(p.blocks[taken].insts.iter().any(|i| i.name() == "v_readfirstlane_b32"));
}

#[test]
fn a_wave_reduction_in_a_loop_the_lanes_leave_one_by_one_translates_in_whole_wave_flow() {
    // a do-while whose passes ORs the lanes' bits over the wave, then drops the lanes past s0:
    // per lane, the later passes are not every lane's (the latch's own block is in its
    // region); in whole-wave flow both halves run every pass and meet in each
    let mut body = vec![
        0x7E0E_0300, // v_mov_b32 v7, v0
        0x7E10_0280, // v_mov_b32 v8, 0
        0xBE8C_047E, // s_mov_b64 s[12:13], exec
    ];
    body.extend_from_slice(&super::wave::WAVE_OR_V7_TO_S7); // (loop:)
    body.extend_from_slice(&[
        0x3810_1007, // v_or_b32 v8, s7, v8
        0x4A0E_0E81, // v_add_nc_u32 v7, 1, v7
        0x7D88_0E00, // v_cmp_gt_u32 vcc, s0, v7
        0x87FE_6A7E, // s_and_b64 exec, exec, vcc
        0xBF89_FFE7, // s_cbranch_execnz loop
        0xBEFE_040C, // s_mov_b64 exec, s[12:13]
        0xE070_0000,
        0x8002_0800, // buffer_store_dword v8, off, s[8:11], 0
    ]);
    let (p, st, barriers) = refused_then_literal(&body);
    let latch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == "s_cbranch_execnz"))
        .expect("the latch");
    assert!(st.wide.contains(&latch));
    // the reduction's exchange and the exit's vote, both in the loop's block
    assert!(barriers.iter().filter(|&&(b, _)| b == latch).count() >= 4, "{barriers:?}");
}

#[test]
fn a_barrier_in_a_lane_branch_is_met_by_both_halves() {
    // `if (t < s0) { readfirstlane; s_barrier; store }`: per lane the barrier would wait for
    // lanes that skipped the branch; in whole-wave flow every lane gets to it, unguarded
    let (p, _, barriers) = refused_then_literal(&with_lanes(&[
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0005, // s_cbranch_execz skip
        0x7E0C_0500, // v_readfirstlane_b32 s6, v0
        0xBF8A_0000, // s_barrier
        0x7E02_0206, // v_mov_b32 v1, s6
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
        0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
    ]));
    let inside = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "s_barrier"))
        .expect("the body");
    // the first-lane read's exchange (at least two) and the program's own barrier
    assert!(barriers.iter().filter(|&&(b, _)| b == inside).count() >= 3, "{barriers:?}");
}

#[test]
fn an_end_in_a_lane_branch_ends_both_halves_together() {
    // `if (t < s0) { readfirstlane; store; s_endpgm }`: the console's wave ends there whatever
    // EXEC holds; in whole-wave flow so do both host waves, and no barrier follows
    let (p, _, barriers) = refused_then_literal(&with_lanes(&[
        0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0005, // s_cbranch_execz skip
        0x7E0C_0500, // v_readfirstlane_b32 s6, v0
        0x7E02_0206, // v_mov_b32 v1, s6
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
        S_ENDPGM,
        0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
        0x7E02_0287, // v_mov_b32 v1, 7
        0xE070_0008,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0 offset:8
    ]));
    let ended = (0..p.blocks.len())
        .find(|&b| p.blocks[b].exit == ir::Exit::End && p.blocks[b].insts.iter().any(|i| i.name() == "v_readfirstlane_b32"))
        .expect("the branch's end");
    let skip = (0..p.blocks.len())
        .find(|&b| p.blocks[b].exit == ir::Exit::End && b != ended)
        .expect("the other end");
    // the two ends are apart: no barrier of one waits for the other's threads
    assert!(barriers.iter().all(|&(b, _)| b != skip), "{barriers:?}");
}

#[test]
fn an_lds_store_under_a_guard_meets_outside_the_guard() {
    // `if (t < s0) { lds[t] = readfirstlane(t) }`, then every lane reads its neighbour's: the
    // store stays under EXEC, the host waves' meeting before it does not
    let native = native_lds(
        &with_lanes(&[
            0x3408_0082, // v_lshlrev_b32 v4, 2, v0
            0x7D88_0000, // v_cmp_gt_u32 vcc, s0, v0
            0xBE8C_246A, // s_and_saveexec_b64 s[12:13], vcc
            0xBF88_0004, // s_cbranch_execz skip
            0x7E0C_0500, // v_readfirstlane_b32 s6, v0
            0x7E02_0206, // v_mov_b32 v1, s6
            0xD834_0000,
            0x0000_0104, // ds_write_b32 v4, v1
            0xBEFE_040C, // s_mov_b64 exec, s[12:13] (skip:)
            0xD8D8_0004,
            0x0100_0004, // ds_read_b32 v1, v4 offset:4
            0xE070_0000,
            0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
        ]),
        64,
        512,
    );
    let (p, st, barriers) = refused_then_literal_of(&native);
    let store = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "ds_write_b32"))
        .expect("the store's block");
    let at = p.blocks[store].insts.iter().position(|i| i.name() == "ds_write_b32").expect("the store");
    assert!(st.guards[store][at].is_some(), "the store keeps EXEC as its guard");
    assert!(barriers.iter().any(|&(b, _)| b == store), "the halves meet before it: {barriers:?}");
}

#[test]
fn whole_wave_flow_with_a_lane_branch_left_per_lane_is_refused() {
    // (the per-lane structure marked literal: a block only some lanes run is refused, not
    // lowered with barriers the halves could part around)
    let native = native(&with_lanes(&FIRST_LANE_IN_BRANCH), 64);
    let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
    let mut st = structure::structure(&p).expect("structure");
    st.literal = true;
    match lower_with(&native, &p, &st, 32) {
        Err(ShaderError::Unsupported { what, .. }) => assert_eq!(what, lower::literal::NOT_WHOLE),
        other => panic!("expected {:?}: {other:?}", lower::literal::NOT_WHOLE),
    }
}

#[test]
fn a_branch_back_to_two_loop_headers_counts_both_loops() {
    // (#593's second shape) two nested loops sharing a latch: the entry of the outer loop
    // jumps forward into the inner one, whose body falls into the latch; the latch goes back
    // to the outer header when no lane stays (`vccz`), else falls through, back to the inner
    // one. The inner body reads the first lane.
    let body = [
        0xBE86_0380, // s_mov_b32 s6, 0
        0x7E02_0280, // v_mov_b32 v1, 0
        0xBF09_0106, // s_cmp_ge_u32 s6, s1 (outer:)
        0xBF85_0009, // s_cbranch_scc1 done
        0x8006_8106, // s_add_u32 s6, s6, 1
        0x7E04_0280, // v_mov_b32 v2, 0
        0xBF82_0002, // s_branch inner
        0x7D88_0500, // v_cmp_gt_u32 vcc, v0, v2 (latch:)
        0xBF86_FFF9, // s_cbranch_vccz outer
        0x7E0E_0500, // v_readfirstlane_b32 s7, v0 (inner:)
        0x4A02_0207, // v_add_nc_u32 v1, s7, v1
        0x4A04_0481, // v_add_nc_u32 v2, 1, v2
        0xBF82_FFFA, // s_branch latch
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0 (done:)
    ];
    let (p, st, _) = refused_then_literal(&with_lanes(&body));
    let latch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.last().is_some_and(|i| i.name() == "s_cbranch_vccz"))
        .expect("the latch");
    assert!(st.wide.contains(&latch));
    // both of its targets are loop headers it jumps back to
    let order = ir::reverse_postorder(&p.blocks);
    let rank = |b: usize| order.iter().position(|&x| x == b).expect("reachable");
    let ir::Exit::Branch { taken, not_taken } = p.blocks[latch].exit else { panic!("a branch") };
    assert!(rank(taken) <= rank(latch) && rank(not_taken) <= rank(latch));
}

/// A do-while loop the lanes leave one by one, whose latch is its only block, with a
/// first-lane read in it: per lane, its later passes are not every lane's.
#[test]
fn a_first_lane_read_in_a_do_while_loop_translates_in_whole_wave_flow() {
    refused_then_literal(&with_lanes(&[
        0x7E02_0280, // v_mov_b32 v1, 0
        0x7E04_0280, // v_mov_b32 v2, 0
        0x7E0E_0500, // v_readfirstlane_b32 s7, v0 (inner:)
        0x4A02_0207, // v_add_nc_u32 v1, s7, v1
        0x4A04_0481, // v_add_nc_u32 v2, 1, v2
        0x7D88_0500, // v_cmp_gt_u32 vcc, v0, v2
        0xBF87_FFFB, // s_cbranch_vccnz inner
        0xE070_0000,
        0x8002_0100, // buffer_store_dword v1, off, s[8:11], 0
    ]));
}

#[test]
fn whole_wave_flow_is_only_for_one_console_wave_on_a_host_of_32_lanes() {
    let one = native(&FIRST_LANE_IN_BRANCH, 64);
    assert!(lower::literal::applies(&one, 32));
    // a 64-lane host runs the console's waves as they are: nothing changes there
    assert!(!lower::literal::applies(&one, 64));
    assert!(!lower::literal::applies(&one, 0));
    // several console waves in a group would vote apart
    assert!(!lower::literal::applies(&native(&FIRST_LANE_IN_BRANCH, 128), 32));
    assert!(!lower::literal::applies(&native(&FIRST_LANE_IN_BRANCH, 32), 32));
    let mut wave32 = native(&FIRST_LANE_IN_BRANCH, 64);
    wave32.wave64 = false;
    assert!(!lower::literal::applies(&wave32, 32));
}

#[test]
fn on_a_host_of_64_lanes_the_per_lane_form_is_unchanged() {
    // the per-lane structure is what it was (no branch taken wave-wide), and a host of 64-lane
    // waves translates the shapes per lane, in the console's own waves
    for body in [&FIRST_LANE_IN_BRANCH[..], &WHILE_FIRST_LANE[..]] {
        let native = native(&with_lanes(body), 64);
        let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
        let st = structure::structure(&p).expect("structure");
        assert!(!st.literal && st.wide.is_empty());
        let (wave, barriers) = lower_with(&native, &p, &st, 64).expect("per lane");
        assert_eq!(wave, Some(64));
        assert!(barriers.is_empty(), "the console's own waves meet nowhere: {barriers:?}");
    }
}

#[test]
fn a_barrier_under_a_guard_is_refused_at_the_end_of_lowering() {
    // (no lowering path makes one: a fault injected into every guarded region, here the
    // branch's store, reaches the refusal through the whole lowering)
    let native = native(&with_lanes(&FIRST_LANE_IN_BRANCH), 64);
    let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
    let st = structure::structure_literal(&p).expect("structure_literal");
    assert!(lower_with(&native, &p, &st, 32).is_ok());
    let was = lower::literal::INJECT_GUARDED_BARRIER.replace(true);
    let r = lower_with(&native, &p, &st, 32);
    lower::literal::INJECT_GUARDED_BARRIER.set(was);
    match r {
        Err(ShaderError::Unsupported { what, .. }) => assert_eq!(what, lower::literal::BARRIER_GUARDED),
        other => panic!("expected {:?}: {other:?}", lower::literal::BARRIER_GUARDED),
    }
}

#[test]
fn a_do_while_latch_on_a_host_of_64_lanes_keeps_its_barrier_and_permlane() {
    // a per-lane do-while whose latch holds an `s_barrier` and a fetch-inactive permlane: on
    // a 64-lane host the latch counts as every lane's, as before whole-wave flow, so the
    // barrier still waits (not a fence alone) and the permlane is not refused
    let native = native(
        &[
            0x7E04_0280, // v_mov_b32 v2, 0
            0xD778_0804,
            0x0201_0100, // v_permlanex16_b32 v4, v0, 0, 0 op_sel:[1,0] (loop:)
            0xBF8A_0000, // s_barrier
            0x4A04_0481, // v_add_nc_u32 v2, 1, v2
            0x7D88_0500, // v_cmp_gt_u32 vcc, v0, v2
            0xBF87_FFFA, // s_cbranch_vccnz loop
            0xE070_0000,
            0x8002_0400, // buffer_store_dword v4, off, s[8:11], 0
        ],
        64,
    );
    let p = ir::build(&native.code, true, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let latch = (0..p.blocks.len())
        .find(|&b| p.blocks[b].insts.iter().any(|i| i.name() == "s_barrier"))
        .expect("the latch");
    let (wave, barriers) = lower_with(&native, &p, &st, 64).expect("per lane on 64 lanes");
    assert_eq!(wave, Some(64));
    assert_eq!(barriers, vec![(latch, false)], "the latch's barrier waits");
}
