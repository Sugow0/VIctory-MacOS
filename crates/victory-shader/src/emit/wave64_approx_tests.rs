//! `VICTORY_WAVE64_APPROX=1` ([`super::wave64_approx`]): a console wave64 compute program
//! that no exact or approximate form fits into host waves of 32 is refused without it,
//! translated with it (two waves of 32 that do not exchange), and keyed apart in the cache.

use super::{HostShader, lower};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{ShaderError, io, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;
const DS_WRITE_B32: u8 = 0x0D;
const DS_READ_B32: u8 = 0x36;

/// DS `op vdst, addr, data0` at offset 0.
const fn ds(op: u8, vdst: u8, addr: u8, data0: u8) -> [u32; 2] {
    [0x36 << 26 | (op as u32) << 18, (vdst as u32) << 24 | (data0 as u32) << 8 | addr as u32]
}

/// A group of 128 threads (two console waves) with 512 bytes of LDS, after wave64_tests'
/// `flag_program(Some(2))`, reading its console lane (`v_mbcnt`, which needs 64-lane waves;
/// a group of one console wave now takes whole-wave flow, `literal`):
/// the threads below s0 raise an LDS flag, read it back and write a second value inside their
/// branch, so the two host waves would have to share LDS in a lane branch, and a scalar the
/// branch sets is read after it: no form fits that.
/// Without `scalar` the skip around the branch can be left out, and the program translates
/// exactly, in two waves of 32 with a group barrier before every LDS access.
fn program(scalar: bool) -> NativeProgram {
    let mut body = vec![
        0xD765_000A,
        0x0001_00C1, // v_mbcnt_lo_u32_b32 v10, -1, 0
        0xD766_000A,
        0x0002_14C1, // v_mbcnt_hi_u32_b32 v10, -1, v10 (the console lane: 64-lane waves)
        0x7E12_0280, // v_mov_b32 v9, 0
    ];
    body.extend_from_slice(&ds(DS_WRITE_B32, 0, 9, 9));
    // (a scalar the branch changes and the code after it reads keeps its skip in every form)
    body.push(0xBE94_0380); // s_mov_b32 s20, 0
    let mut branch = vec![0x7E04_0281]; // v_mov_b32 v2, 1
    if scalar {
        branch.push(0xBE94_0381); // s_mov_b32 s20, 1
    }
    branch.extend_from_slice(&ds(DS_WRITE_B32, 0, 9, 2));
    let mut rest = ds(DS_READ_B32, 3, 9, 0).to_vec();
    rest.extend_from_slice(&[0x7E0C_02FF, 2]); // v_mov_b32 v6, 2
    rest.extend_from_slice(&ds(DS_WRITE_B32, 0, 9, 6));
    branch.extend_from_slice(&[0xBF07_8001, 0xBF85_0000 | rest.len() as u32]); // s_cmp_lg_u32 s1, 0; s_cbranch_scc1
    branch.extend_from_slice(&rest);
    body.extend_from_slice(&[
        0x7D88_0000,                       // v_cmp_gt_u32 vcc, s0, v0
        0xBE8C_246A,                       // s_and_saveexec_b64 s[12:13], vcc
        0xBF88_0000 | branch.len() as u32, // s_cbranch_execz past the branch
    ]);
    body.extend_from_slice(&branch);
    body.push(0xBEFE_040C); // s_mov_b64 exec, s[12:13]
    body.extend_from_slice(&ds(DS_READ_B32, 5, 9, 0));
    body.extend_from_slice(&[
        0x4A0A_0A81, // v_add_nc_u32 v5, 1, v5
        0x4A0A_0A14, // v_add_nc_u32 v5, s20, v5
        0x4A0A_0B0A, // v_add_nc_u32 v5, v10, v5
        0x3402_0082, // v_lshlrev_b32 v1, 2, v0
        0x38 << 26 | 0x1C << 18 | 1 << 12,
        0x80 << 24 | 2 << 16 | 5 << 8 | 1, // buffer_store_dword v5, v1, s[8:11], 0 offen
    ]);
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(&body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam { root: RootType::Constants, table_offset_dwords: 0, sgprs: 4, base_register: 0, user_data: 0, slot: 0 },
            RootParam { root: RootType::Table, table_offset_dwords: 0, sgprs: 1, base_register: 0, user_data: 4, slot: 1 },
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
        io: StageIo::Cs(ComputeIo { rsrc2: 5 << 1 | 1 << 15, threads: [128, 1, 1] }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// [`super::emit`] (the whole retry chain) on a host of 32-lane waves, the switch as `approx`.
fn emitted(approx: bool) -> Result<HostShader, ShaderError> {
    emitted_of(&program(true), approx)
}

fn emitted_of(native: &NativeProgram, approx: bool) -> Result<HostShader, ShaderError> {
    let bytes = crate::dxbc::build(&[]);
    let c = super::Container::parse(&bytes).expect("container");
    let p = crate::spill::build(native).expect("build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource::track");
    let io = io::raise(&p, native).expect("io::raise");
    let wave = lower::THREAD_MAX_WAVE.replace(Some(32));
    let was = super::WAVE64_APPROX_HERE.replace(Some(approx));
    let out = super::emit(&c, native, &p, &st, &io, &res);
    super::WAVE64_APPROX_HERE.set(was);
    lower::THREAD_MAX_WAVE.set(wave);
    out
}

#[test]
fn a_refused_wave64_program_translates_approximately_when_asked() {
    match emitted(false) {
        Err(ShaderError::Unsupported { what, .. }) => assert_eq!(what, super::WIDER_THAN_HOST),
        Err(e) => panic!("refused for another reason: {e}"),
        Ok(s) => panic!("translated without the switch (wave {:?})", super::wanted_wave(&s.dxil)),
    }
    let shader = emitted(true).expect("translated with VICTORY_WAVE64_APPROX=1");
    assert_eq!(super::wanted_wave(&shader.dxil), Some(32));
    assert!(super::within_host_wave(shader, 32).is_ok());
}

#[test]
fn approximate_translations_are_keyed_apart() {
    let key = |approx: bool| {
        let was = super::WAVE64_APPROX_HERE.replace(Some(approx));
        let k = super::host_settings();
        super::WAVE64_APPROX_HERE.set(was);
        k
    };
    let (off, on) = (key(false), key(true));
    assert_eq!(off.len(), 6, "without the switch the key is as before");
    assert_ne!(off, on);
    assert_eq!(on[..6], off[..]);
}

/// The group barriers in a translation (`dx.op.barrier` calls, by the module's census).
fn barriers(shader: &HostShader) -> u32 {
    let at = super::bitcode::census::KINDS.iter().position(|k| *k == "barrier").expect("kind");
    super::bitcode::census::take(&shader.dxil).expect("the census of the kept module").counts[at]
}

#[test]
fn an_approximate_translation_adds_no_barrier() {
    let was = super::bitcode::census::COUNT_HERE.replace(true);
    // (the count sees barriers: the exact form of the program without the scalar has its LDS
    // accesses' group barriers)
    let exact = emitted_of(&program(false), false).expect("the exact form");
    let exact_barriers = barriers(&exact);
    let approx = emitted(true).expect("translated with VICTORY_WAVE64_APPROX=1");
    let approx_barriers = barriers(&approx);
    super::bitcode::census::COUNT_HERE.set(was);
    assert_eq!(super::wanted_wave(&exact.dxil), Some(32));
    assert!(exact_barriers > 1, "no LDS barrier counted in the exact form");
    // the console program has no `s_barrier`: only the entry block's, where every thread of
    // the group meets once to number the host waves (`wave_base`, in every split into waves
    // of 32, before any branch), and none for LDS or in a branch
    assert_eq!(approx_barriers, 1);
}

#[test]
fn only_1_turns_the_switch_on() {
    assert!(super::approx_asked(Some("1")));
    assert!(super::approx_asked(Some(" 1 ")));
    for off in [None, Some("0"), Some(""), Some("true"), Some("2")] {
        assert!(!super::approx_asked(off), "{off:?}");
    }
}

#[test]
fn a_program_that_translates_exactly_stays_exact_with_the_switch() {
    let off = emitted_of(&program(false), false).expect("exact");
    let on = emitted_of(&program(false), true).expect("exact with the switch");
    assert_eq!(off.dxil, on.dxil);
}
