//! Mesh and pixel shaders linked (`mesh_link`), on synthetic programs.
//! The two programs of the first tests are pragyaangaur/VIctory-macOS
//! PR #161's, those of the interleaved one PR #401's.

use super::container::{self, SigElement};
use super::lower::TEST_METAL_MESH_LIMIT;
use super::mesh_link::{
    KeptOutputs, Linked, STILL_OVER, TEST_MESH_LINK, kept_outputs, layout, link_with,
    mesh_fits_metal, pixel_input_reads, prune_signature, translate_linked, with_linked,
};
use super::*;
use crate::native::{MeshIo, PsInput, StageIo, Usage, VsOutput};

/// Triangle (0, 1, 2), each corner's 9-bit index in its 10-bit field.
const TRIANGLE: u32 = 1 << 10 | 2 << 20;

fn element(name: &str, index: u32, kind: u8, row: u32, cols: u8, interp: u8) -> SigElement {
    SigElement {
        name: name.into(),
        index,
        kind,
        comp: 9,
        interp,
        row: Some(row),
        col: 0,
        cols,
        usage: 0xF,
        half: false,
    }
}

fn position() -> SigElement {
    element("SV_Position", 0, 3, 0, 4, 0)
}

/// The arbitrary elements of a translated container's signature part, as
/// (name, index, channels), in the part's order.
fn arbitrary(dxil: &[u8], fourcc: FourCC) -> Vec<(String, u32, u8)> {
    let c = Container::parse(dxil).unwrap();
    super::signature(c.part(fourcc).unwrap().data)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == 0)
        .map(|e| (e.name, e.index, e.cols))
        .collect()
}

/// D3D12's count of a translated mesh shader's outputs at its maxima.
fn packed_bytes(dxil: &[u8]) -> u64 {
    let c = Container::parse(dxil).unwrap();
    let sig = |f| super::signature(c.part(f).unwrap().data).unwrap();
    let psv = c.part(FourCC::PSV0).unwrap().data;
    let half = |at: usize| u32::from(u16::from_le_bytes([psv[4 + at], psv[5 + at]]));
    container::mesh_output_bytes(&sig(FourCC::OSG1), &sig(FourCC::PSG1), half(12), half(14))
}

/// A program and its container, run through the stages before emission.
struct Program {
    c: Vec<u8>,
    native: NativeProgram,
}

impl Program {
    fn emit(&self) -> Result<HostShader, ShaderError> {
        let c = Container::parse(&self.c)?;
        let p = ir::build(
            &self.native.patched_code(),
            self.native.wave64,
            self.native.vgprs,
        )?;
        let st = structure::structure(&p)?;
        let res = resource::track(&p, &self.native.layout, self.native.stage, Some(&st))?;
        let io = io::raise(&p, &self.native)?;
        emit(&c, &self.native, &p, &st, &io, &res)
    }
}

/// D3DMetal (`metal`) with linking on (`link`), on this thread.
fn host(metal: bool, link: bool) {
    TEST_METAL_MESH_LIMIT.set(Some(metal));
    TEST_MESH_LINK.set(Some(link));
}

/// PR #161's mesh program: `threads` lanes, `SV_Position` and 16 `float4`
/// `TEXCOORD`s (68 components): TEXCOORD0 = (1, 0.5, 0, 0), TEXCOORD15 =
/// (0, 0, 0.25, 1), the others 0. 34,816 bytes at 128 vertices.
fn wide_mesh(threads: u32, max_vertices: u32, max_primitives: u32) -> Program {
    wide_mesh_sized(threads, max_vertices, max_primitives, &[])
}

/// [`wide_mesh`] at 128 lanes with `extra` output elements declared.
fn wide_mesh_with(extra: &[SigElement]) -> Program {
    wide_mesh_sized(128, 128, 128, extra)
}

fn wide_mesh_sized(
    threads: u32,
    max_vertices: u32,
    max_primitives: u32,
    extra: &[SigElement],
) -> Program {
    let mut words = vec![
        0xD765_0001u32, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0x0001_00C1,
        0x3604_0281, // v_and_b32 v2, 1, v1
        0x3606_0282, // v_and_b32 v3, 2, v1
        0x7E04_0D02, // v_cvt_f32_u32 v2, v2
        0x7E06_0D03, // v_cvt_f32_u32 v3, v3
        0x1008_04F6, // v_mul_f32 v4, 4.0, v2
        0x0608_08F3, // v_add_f32 v4, -1.0, v4
        0x100A_06F4, // v_mul_f32 v5, 2.0, v3
        0x060A_0AF3, // v_add_f32 v5, -1.0, v5
        0x7E0C_02F0, // v_mov_b32 v6, 0.5
        0x7E0E_02F2, // v_mov_b32 v7, 1.0
        0x7E10_02FF, // v_mov_b32 v8, 0.25
        0.25f32.to_bits(),
        0x7E12_02FF, // v_mov_b32 v9, TRIANGLE
        TRIANGLE,
        0x7E14_0280, // v_mov_b32 v10, 0.0
    ];
    // param0 = (1.0, 0.5, 0.0, 0.0)
    words.extend([0xF800_000F | (32 << 4), 0x0A0A_0607]);
    for k in 1..15u32 {
        words.extend([0xF800_000F | ((32 + k) << 4), 0x0A0A_0A0A]);
    }
    // param15 = (0.0, 0.0, 0.25, 1.0)
    words.extend([0xF800_000F | ((32 + 15) << 4), 0x0708_0A0A]);
    words.extend([
        0xF800_0941, // exp prim v9, off, off, off done
        0x0000_0009,
        0xF800_08CF, // exp pos0 v4, v5, v6, v7 done
        0x0706_0504,
        0xBF81_0000, // s_endpgm
    ]);
    let outputs = (0..16u8)
        .map(|k| VsOutput {
            usage: Usage::Generic,
            row: k + 1,
            param: k,
            fp16: false,
            pack2: false,
        })
        .collect();
    let native = NativeProgram {
        stage: HwStage::Ms,
        wave64: false,
        vgprs: 12,
        layout: vec![],
        used: vec![],
        io: StageIo::Ms(MeshIo {
            outputs,
            clip_cntl: 0,
            threads,
            max_vertices,
            max_primitives,
            topology: 2,
            lds_bytes: 0,
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let mut sig = vec![position()];
    sig.extend((0..16).map(|k| element("TEXCOORD", k, 0, k + 1, 4, 0)));
    sig.extend(extra.iter().cloned());
    let osg1 = container::signature(&sig, false);
    Program {
        c: crate::dxbc::build(&[(FourCC::OSG1, &osg1)]),
        native,
    }
}

/// `v_interp_p1_f32` and `v_interp_p2_f32` of attribute `attr`, channel
/// `ch`, into `vdst`.
fn interp(vdst: u32, attr: u32, ch: u32) -> [u32; 2] {
    [
        0xC800_0000u32 | (vdst << 18) | (attr << 10) | (ch << 8),
        0xC801_0001u32 | (vdst << 18) | (attr << 10) | (ch << 8),
    ]
}

/// A pixel program declaring `SV_Position` and `TEXCOORD0..15` (rows 1 to
/// 16) and, per `extra`, more inputs; its code is `body` then
/// `exp mrt0 v2, v3, v4, v5`.
fn pixel(body: &[u32], extra: &[(PsInput, SigElement)]) -> Program {
    pixel_with(body, extra, &[])
}

/// [`pixel`] with the system values `sys` declared too.
fn pixel_with(body: &[u32], extra: &[(PsInput, SigElement)], sys: &[SigElement]) -> Program {
    let mut words = body.to_vec();
    words.extend([
        0xF800_180F,
        0x0504_0302, // exp mrt0, v2, v3, v4, v5 done vm
        0xBF81_0000, // s_endpgm
    ]);
    let mut inputs: Vec<PsInput> = (0..16u8)
        .map(|k| PsInput {
            usage: Usage::Generic,
            row: k + 1,
            mask: 0xF,
            attr: k,
            flat: false,
            fp16: false,
            pack2: false,
            row2: 0,
        })
        .collect();
    let mut sig = vec![SigElement {
        usage: 0,
        ..position()
    }];
    sig.extend((0..16).map(|k| SigElement {
        usage: 0,
        ..element("TEXCOORD", k, 0, k + 1, 4, 0)
    }));
    for (input, e) in extra {
        inputs.push(*input);
        sig.push(e.clone());
    }
    sig.extend(sys.iter().cloned());
    let native = NativeProgram {
        stage: HwStage::Ps,
        wave64: false,
        vgprs: 8,
        layout: vec![],
        used: vec![],
        io: StageIo::Ps {
            inputs,
            input_addr: 1 << 1,
            db_shader_control: 0,
            patches: vec![],
        },
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let isg1 = container::signature(&sig, true);
    let osg1 = container::signature(&[element("SV_Target", 0, 16, 0, 4, 0)], false);
    Program {
        c: crate::dxbc::build(&[(FourCC::ISG1, &isg1), (FourCC::OSG1, &osg1)]),
        native,
    }
}

/// PR #161's pixel program: reads TEXCOORD0.xy and TEXCOORD15.zw.
fn corner_reader() -> Program {
    let body: Vec<u32> = [
        interp(2, 0, 0),
        interp(3, 0, 1),
        interp(4, 15, 2),
        interp(5, 15, 3),
    ]
    .concat();
    pixel(&body, &[])
}

/// Pruning keeps an arbitrary element only with a channel read, trimmed to
/// the channels read (relative to the element), system values untouched,
/// and orders the kept ones by size: 4, 2, 1 then 3 channels.
#[test]
fn pruning_keeps_exactly_the_channels_read() {
    let mut els = vec![
        element("TEXCOORD", 0, 0, 1, 4, 2),
        position(),
        element("TEXCOORD", 1, 0, 2, 4, 2),
        SigElement {
            col: 2,
            ..element("COLOR", 0, 0, 3, 2, 2)
        },
        element("TEXCOORD", 2, 0, 4, 3, 2),
        element("TEXCOORD", 3, 0, 5, 4, 2),
    ];
    let keep: KeptOutputs = [
        (("TEXCOORD".into(), 0), 0b0010), // .y alone
        (("TEXCOORD".into(), 2), 0b0111), // all three
        (("COLOR".into(), 0), 0b0011),    // its own .xy: register .zw
        (("TEXCOORD".into(), 3), 0b1111),
        (("TEXCOORD".into(), 9), 0b1111), // not declared
    ]
    .into();
    prune_signature(&mut els, &keep);
    let got: Vec<_> = els
        .iter()
        .map(|e| (e.name.as_str(), e.index, e.col, e.cols))
        .collect();
    assert_eq!(
        got,
        [
            ("SV_Position", 0, 0, 4),
            ("TEXCOORD", 3, 0, 4),
            ("COLOR", 0, 2, 2),
            ("TEXCOORD", 0, 1, 1),
            ("TEXCOORD", 2, 0, 3),
        ]
    );
    // Nothing read: the system values alone.
    let mut els = vec![position(), element("TEXCOORD", 0, 0, 1, 4, 2)];
    prune_signature(&mut els, &KeptOutputs::new());
    assert_eq!(els, [position()]);
}

/// The layout puts the system values a register each first, then packs
/// the arbitrary elements in order into registers of four channels of one
/// mode (the pixel shader's where it has one) and kind; the primitive
/// outputs follow the vertex outputs.
#[test]
fn layout_fills_registers_in_element_order() {
    let vertex = vec![
        SigElement {
            row: Some(7),
            ..position()
        },
        element("A", 0, 0, 9, 4, 2),
        element("B", 0, 0, 3, 2, 2),
        element("C", 0, 0, 5, 1, 2),
        element("D", 0, 0, 1, 1, 2), // constant in the pixel shader
        element("E", 0, 0, 2, 1, 2),
    ];
    let prims = vec![element("P", 0, 0, 0, 2, 1), element("Q", 0, 0, 1, 2, 1)];
    let modes = [(("D".to_owned(), 0), 1)].into();
    let l = layout(&vertex, &prims, &modes);
    let at = |n: &str| l.at[&(n.to_owned(), 0)];
    assert_eq!(at("SV_POSITION"), (0, 0, 0));
    assert_eq!(at("A"), (1, 0, 2));
    assert_eq!(at("B"), (2, 0, 2));
    assert_eq!(at("C"), (2, 2, 2));
    assert_eq!(at("D"), (3, 0, 1));
    assert_eq!(at("E"), (4, 0, 2));
    assert_eq!(l.prim_base, 5);
    assert_eq!(at("P"), (5, 0, 1));
    assert_eq!(at("Q"), (5, 2, 1));
}

/// The registers and first channels of a translated container's elements
/// in `fourcc`, by semantic, `base` added to the registers.
fn registers(dxil: &[u8], fourcc: FourCC, base: u32) -> Vec<((String, u32), (u32, u8), u8)> {
    let c = Container::parse(dxil).unwrap();
    match c.part(fourcc) {
        Some(part) => super::signature(part.data)
            .unwrap()
            .into_iter()
            .map(|e| {
                (
                    (e.name.to_ascii_uppercase(), e.index),
                    (e.row.unwrap() + base, e.col),
                    e.kind,
                )
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Every input of the linked pixel shader the mesh shader outputs sits at
/// the same register and channel as that output (vertex outputs, then the
/// primitive outputs after them); its other inputs (`own`) after them all.
fn assert_same_registers(ms: &[u8], ps: &[u8], own: &[&str]) {
    let vertex = registers(ms, FourCC::OSG1, 0);
    let base = vertex.iter().map(|e| e.1.0 + 1).max().unwrap_or(0);
    let mut outputs = vertex;
    outputs.extend(registers(ms, FourCC::PSG1, base));
    let last = outputs.iter().map(|e| e.1.0).max().unwrap_or(0);
    for (key, at, _) in registers(ps, FourCC::ISG1, 0) {
        match outputs.iter().find(|o| o.0 == key) {
            Some(o) => assert_eq!(o.1, at, "{key:?}"),
            None => {
                assert!(own.contains(&key.0.as_str()), "{key:?} has no output");
                assert!(at.0 > last, "{key:?} at {at:?} among the outputs");
            }
        }
    }
}

/// PR #161: a 128-lane mesh shader over 32 KiB at one vertex per lane. On
/// D3DMetal without linking it is refused as before; with linking it is
/// translated alone (its pipeline decides), still over, and linked with
/// its pixel shader it keeps SV_Position, TEXCOORD0.xy and TEXCOORD15.zw
/// alone (8 components, 4,096 bytes), and the pixel shader the same inputs.
#[test]
fn over_alone_fits_linked_with_exactly_the_inputs_read() {
    let ms = wide_mesh(128, 128, 128);
    let ps = corner_reader();
    host(true, false);
    let refused = ms.emit();
    assert!(
        matches!(&refused, Err(ShaderError::Unsupported { what, .. }) if what.contains("32 KiB")),
        "{refused:?}"
    );
    host(true, true);
    let alone = ms.emit().unwrap();
    assert_eq!(packed_bytes(&alone.dxil), 34_816);
    assert_eq!(mesh_fits_metal(&alone.dxil), Some(false));

    let full = ps.emit().unwrap();
    let reads = pixel_input_reads(&full.dxil).unwrap();
    assert_eq!(
        reads,
        [
            (("TEXCOORD".into(), 0), 0b0011),
            (("TEXCOORD".into(), 15), 0b1100)
        ]
        .into()
    );

    let (lms, lps) = link_with(&ms.c, || ms.emit(), Some(|| ps.emit())).unwrap();
    let lps = lps.unwrap();
    assert_eq!(mesh_fits_metal(&lms.dxil), Some(true));
    assert_eq!(packed_bytes(&lms.dxil), 8 * 4 * 128);
    let kept = [
        ("TEXCOORD".to_owned(), 0, 2),
        ("TEXCOORD".to_owned(), 15, 2),
    ];
    assert_eq!(arbitrary(&lms.dxil, FourCC::OSG1), kept);
    assert_eq!(arbitrary(&lps.dxil, FourCC::ISG1), kept);
    assert_same_registers(&lms.dxil, &lps.dxil, &[]);
    // The pixel shader alone is not changed by its pair's translation.
    assert_eq!(arbitrary(&full.dxil, FourCC::ISG1).len(), 16);
    // Nor is anything outside a linked translation.
    assert_eq!(ms.emit().unwrap(), alone);
    #[cfg(windows)]
    if let Ok(mut v) =
        super::validator::Validator::load(super::validator::Validator::default_path().as_deref())
    {
        v.validate(&mut lms.dxil.clone()).unwrap();
        v.validate(&mut lps.dxil.clone()).unwrap();
    }
    host(false, false);
}

/// Without a pixel shader (a depth-only draw) the mesh shader keeps its
/// system values alone.
#[test]
fn linked_without_a_pixel_shader_keeps_the_position_alone() {
    host(true, true);
    let ms = wide_mesh(128, 128, 128);
    let (lms, lps) = link_with(
        &ms.c,
        || ms.emit(),
        None::<fn() -> Result<HostShader, ShaderError>>,
    )
    .unwrap();
    assert!(lps.is_none());
    assert!(arbitrary(&lms.dxil, FourCC::OSG1).is_empty());
    assert_eq!(packed_bytes(&lms.dxil), 4 * 4 * 128);
    host(false, false);
}

/// Linking fails, and the pipeline falls back on the mesh shader alone,
/// when the pixel shader reads every output (still over: refused within
/// the linked translation), or when its pair is no pixel shader.
#[test]
fn linking_fails_when_still_over_or_not_a_pixel_shader() {
    host(true, true);
    let ms = wide_mesh(128, 128, 128);
    let body: Vec<u32> = (0..16u32)
        .flat_map(|attr| (0..4u32).flat_map(move |ch| interp(2 + ch, attr, ch)))
        .collect();
    let all = pixel(&body, &[]);
    let still = link_with(&ms.c, || ms.emit(), Some(|| all.emit()));
    assert!(
        matches!(&still, Err(ShaderError::Unsupported { what, .. }) if what.contains("32 KiB") || *what == STILL_OVER),
        "{still:?}"
    );
    let other = link_with(&ms.c, || ms.emit(), Some(|| ms.emit()));
    assert!(matches!(other, Err(ShaderError::Malformed(_))), "{other:?}");
    // The mesh shader alone still translates.
    assert_eq!(mesh_fits_metal(&ms.emit().unwrap().dxil), Some(false));
    host(false, false);
}

/// PR #401: five 1-component reads ahead of ten 4-component ones and
/// PRIM0.x ahead of PRIM1.xyzw put Metal's aligned layout of the kept
/// outputs over 32 KiB in signature order; linked, the outputs are ordered
/// by size and repacked, and the layout fits. PRIM2 and TEXCOORD15 go.
#[test]
fn linked_outputs_are_ordered_by_size() {
    host(true, true);
    let mut words = vec![
        0xD765_0001u32, // v_mbcnt_lo_u32_b32 v1, -1, 0
        0x0001_00C1,
        0x3604_0281, // v_and_b32 v2, 1, v1
        0x3606_0282, // v_and_b32 v3, 2, v1
        0x7E04_0D02, // v_cvt_f32_u32 v2, v2
        0x7E06_0D03, // v_cvt_f32_u32 v3, v3
        0x1008_04F6, // v_mul_f32 v4, 4.0, v2
        0x0608_08F3, // v_add_f32 v4, -1.0, v4
        0x100A_06F4, // v_mul_f32 v5, 2.0, v3
        0x060A_0AF3, // v_add_f32 v5, -1.0, v5
        0x7E0C_02F0, // v_mov_b32 v6, 0.5
        0x7E0E_02F2, // v_mov_b32 v7, 1.0
        0x7E10_02FF, // v_mov_b32 v8, 0.25
        0.25f32.to_bits(),
        0x7E12_02FF, // v_mov_b32 v9, TRIANGLE
        TRIANGLE,
        0x7E14_0280, // v_mov_b32 v10, 0.0
        0x7E16_02FF, // v_mov_b32 v11, 0.125
        0.125f32.to_bits(),
        0x7E18_02FF, // v_mov_b32 v12, 0.75
        0.75f32.to_bits(),
    ];
    words.extend([0xF800_000F | (32 << 4), 0x0A0A_0A07]);
    words.extend([0xF800_000F | (33 << 4), 0x0A0A_060A]);
    for k in 2..16u32 {
        let src = if k == 9 { 0x0A08_0A0A } else { 0x0A0A_0A0A };
        words.extend([0xF800_000F | ((32 + k) << 4), src]);
    }
    words.extend([0xF800_000F | ((32 + 16) << 4), 0x0A0A_0A0B]);
    words.extend([0xF800_000F | ((32 + 17) << 4), 0x0C0A_0A0A]);
    words.extend([0xF800_000F | ((32 + 18) << 4), 0x0A0A_0A0A]);
    words.extend([
        0xF800_0941, // exp prim v9, off, off, off done
        0x0000_0009,
        0xF800_08CF, // exp pos0 v4, v5, v6, v7 done
        0x0706_0504,
        0xBF81_0000, // s_endpgm
    ]);
    let mut outputs: Vec<VsOutput> = (0..16u8)
        .map(|k| VsOutput {
            usage: Usage::Generic,
            row: k + 1,
            param: k,
            fp16: false,
            pack2: false,
        })
        .collect();
    outputs.extend((0..3u8).map(|p| VsOutput {
        usage: Usage::Generic,
        row: p,
        param: 0x80 | (16 + p),
        fp16: false,
        pack2: false,
    }));
    let native = NativeProgram {
        stage: HwStage::Ms,
        wave64: false,
        vgprs: 16,
        layout: vec![],
        used: vec![],
        io: StageIo::Ms(MeshIo {
            outputs,
            clip_cntl: 0,
            threads: 128,
            max_vertices: 128,
            max_primitives: 128,
            topology: 2,
            lds_bytes: 0,
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let mut sig = vec![position()];
    sig.extend((0..16).map(|k| element("TEXCOORD", k, 0, k + 1, 4, 0)));
    let prims: Vec<SigElement> = (0..3).map(|p| element("PRIM", p, 0, p, 4, 1)).collect();
    let ms = Program {
        c: crate::dxbc::build(&[
            (FourCC::OSG1, &container::signature(&sig, false)),
            (FourCC::PSG1, &container::signature(&prims, false)),
        ]),
        native,
    };
    let pmov =
        |vdst: u32, attr: u32, ch: u32| 0xC802_0002u32 | (vdst << 18) | (attr << 10) | (ch << 8);
    let mut body = vec![0x7E04_0280u32, 0x7E06_0280, 0x7E08_0280, 0x7E0A_0280];
    for attr in [0u32, 2, 4, 6, 8] {
        body.extend(interp(6, attr, 0));
        body.push(0x0604_0D02); // v_add_f32 v2, v2, v6
    }
    for attr in [1u32, 3, 5, 7, 9, 10, 11, 12, 13, 14] {
        for ch in 0..4u32 {
            body.extend(interp(6, attr, ch));
            body.push(match ch {
                1 => 0x0606_0D03,
                2 => 0x0608_0D04,
                _ => 0x060A_0D05,
            });
        }
    }
    body.extend([pmov(6, 16, 0), 0x0608_0D04]);
    for ch in 0..4u32 {
        body.extend([pmov(6, 17, ch), 0x060A_0D05]);
    }
    let extra: Vec<(PsInput, SigElement)> = (0..3u8)
        .map(|p| {
            (
                PsInput {
                    usage: Usage::Generic,
                    row: 17 + p,
                    mask: 0xF,
                    attr: 16 + p,
                    flat: true,
                    fp16: false,
                    pack2: false,
                    row2: 0,
                },
                SigElement {
                    usage: 0,
                    ..element("PRIM", u32::from(p), 0, 17 + u32::from(p), 4, 1)
                },
            )
        })
        .collect();
    let ps = pixel(&body, &extra);

    let alone = ms.emit().unwrap();
    assert_eq!(packed_bytes(&alone.dxil), 40_960);
    assert_eq!(mesh_fits_metal(&alone.dxil), Some(false));
    let (lms, lps) = link_with(&ms.c, || ms.emit(), Some(|| ps.emit())).unwrap();
    assert_eq!(packed_bytes(&lms.dxil), 27_648);
    assert_eq!(mesh_fits_metal(&lms.dxil), Some(true));
    // Kept in signature order: the ten vectors, then the five scalars.
    let vertex = arbitrary(&lms.dxil, FourCC::OSG1);
    let cols: Vec<u8> = vertex.iter().map(|e| e.2).collect();
    assert_eq!(cols, [[4u8; 10].as_slice(), &[1; 5]].concat());
    assert!(!vertex.iter().any(|e| e.1 == 15));
    assert_eq!(
        arbitrary(&lms.dxil, FourCC::PSG1),
        [("PRIM".to_owned(), 1, 4), ("PRIM".to_owned(), 0, 1)]
    );
    // In the console's order the same outputs would not have fit.
    let c = Container::parse(&lms.dxil).unwrap();
    let mut v = super::signature(c.part(FourCC::OSG1).unwrap().data).unwrap();
    let mut p = super::signature(c.part(FourCC::PSG1).unwrap().data).unwrap();
    v[1..].sort_by_key(|e| e.index);
    p.sort_by_key(|e| e.index);
    assert!(container::metal_mesh_output_bytes(&v, &p, 128, 128) > container::MESH_OUTPUT_LIMIT);
    // Per-primitive inputs follow the vertex ones at the mesh shader's
    // registers.
    assert_same_registers(&lms.dxil, &lps.unwrap().dxil, &[]);
    host(false, false);
}

/// System values: the mesh shader's clip distances sit where the pixel
/// shader reads them; the pixel shader's own (front face, primitive id)
/// after every output.
#[test]
fn system_values_share_registers_or_come_after() {
    host(true, true);
    let clip = SigElement {
        comp: 9,
        ..element("SV_ClipDistance", 0, 6, 17, 2, 2)
    };
    let ms = wide_mesh_with(std::slice::from_ref(&clip));
    let uint = |name: &str, kind, row| SigElement {
        comp: 5,
        usage: 0,
        ..element(name, 0, kind, row, 1, 1)
    };
    let sys = [
        SigElement { usage: 0, ..clip },
        uint("SV_IsFrontFace", 13, 18),
        uint("SV_PrimitiveID", 10, 19),
    ];
    let body: Vec<u32> = [
        interp(2, 0, 0),
        interp(3, 0, 1),
        interp(4, 15, 2),
        interp(5, 15, 3),
    ]
    .concat();
    let ps = pixel_with(&body, &[], &sys);
    let (lms, lps) = link_with(&ms.c, || ms.emit(), Some(|| ps.emit())).unwrap();
    let lps = lps.unwrap();
    assert!(
        registers(&lms.dxil, FourCC::OSG1, 0)
            .iter()
            .any(|e| e.2 == 6)
    );
    assert_eq!(registers(&lps.dxil, FourCC::ISG1, 0).len(), 6);
    assert_same_registers(&lms.dxil, &lps.dxil, &["SV_ISFRONTFACE", "SV_PRIMITIVEID"]);
    host(false, false);
}

/// `translate_linked` itself: containers that do not translate are refused,
/// not linked.
#[test]
fn translate_linked_refuses_what_does_not_translate() {
    assert!(translate_linked(b"no container", None).is_err());
    let ms = wide_mesh(128, 128, 128);
    // (a container without the console program)
    assert!(translate_linked(&ms.c, None).is_err());
    assert!(translate_linked(&ms.c, Some(&corner_reader().c)).is_err());
}

/// A linked translation that panics leaves no linking behind on its thread.
#[test]
fn linking_is_undone_when_a_translation_panics() {
    let linked = Linked {
        keep: [(("TEXCOORD".to_owned(), 0), 1)].into(),
        ..Default::default()
    };
    let panicked = std::panic::catch_unwind(|| with_linked(linked, || panic!("in the lowering")));
    assert!(panicked.is_err());
    assert!(kept_outputs().is_none());
}
