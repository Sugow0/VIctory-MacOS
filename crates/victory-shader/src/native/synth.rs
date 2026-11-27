//! Synthetic console shader containers, for tests and test titles: the
//! layout [`super`] reads, written (`XDAT` index, `XBCH`-packed `FSSM`
//! programs, `ISG1`/`OSG1` signatures), around hand-written GFX10 code.
//! Not a compiler: [`vertex_id_triangle`] and [`constant_color`] are the
//! two programs the smoke title draws with (tests/fixtures/smoke-title).
//! Built for this crate's tests and with the `synth` feature.

use super::{
    ABSENT, FLAG_WAVE32, FSSM_HEADER, FSSM_VERSION, GRAPHICS_SLOT_COUNT, HwStage, KIND_NATIVE,
    MRTS, PATCH_BYTES, PS_PATCHES, RECORD_REGS, RECORD_SIZE, SLOTS, VS_OUTPUT_COUNT, XBCH_VERSION,
    XDAT_OFFSET,
};
use crate::dxbc::{FourCC, build};
use crate::emit::container::{SigElement, signature};

/// One program of an `FSSM` blob.
pub struct Program<'a> {
    pub stage: HwStage,
    pub wave32: bool,
    /// Record +0x50: bits 0-9 VGPRs used, bits 20-27 SGPRs used.
    pub regs: u32,
    /// What follows the record (the stage data the parser reads).
    pub stage_data: &'a [u8],
    pub code: &'a [u8],
}

/// An `FSSM` blob: the slot table, then each program's record, its stage
/// data and its code. No root layout or used-resource tables.
pub fn fssm(programs: &[Program]) -> Vec<u8> {
    let slot = |stage| SLOTS.iter().find(|s| s.0 == stage).expect("a slot").1;
    let mut payload = GRAPHICS_SLOT_COUNT.to_le_bytes().to_vec();
    payload.resize(slot(HwStage::Cs) + 8, 0);
    for (_, at) in SLOTS {
        payload[at..at + 8].copy_from_slice(&ABSENT.to_le_bytes());
    }
    for p in programs {
        let at = slot(p.stage);
        let record = payload.len() as u64;
        payload[at..at + 8].copy_from_slice(&record.to_le_bytes());
        let code_at = record + RECORD_SIZE + p.stage_data.len() as u64;
        payload.extend_from_slice(&code_at.to_le_bytes());
        payload.extend_from_slice(&(p.code.len() as u64).to_le_bytes());
        let flags = if p.wave32 { FLAG_WAVE32 } else { 0 };
        payload.extend_from_slice(&[0; 4]);
        payload.extend_from_slice(&flags.to_le_bytes());
        payload.resize(record as usize + RECORD_REGS, 0);
        payload.extend_from_slice(&p.regs.to_le_bytes());
        payload.extend_from_slice(p.stage_data);
        payload.extend_from_slice(p.code);
    }
    let mut out = b"FSSM".to_vec();
    out.extend_from_slice(&FSSM_VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    debug_assert_eq!(out.len(), FSSM_HEADER + payload.len());
    out
}

/// `unpacked` zlib-packed in an `XBCH` wrapper.
pub fn xbch(unpacked: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; zlib_rs::compress_bound(unpacked.len())];
    let (stream, rc) = zlib_rs::compress_slice(&mut buf, unpacked, zlib_rs::DeflateConfig::new(6));
    assert_eq!(rc, zlib_rs::ReturnCode::Ok);
    let mut out = b"XBCH".to_vec();
    out.extend_from_slice(&XBCH_VERSION.to_le_bytes());
    out.extend_from_slice(&(stream.len() as u64).to_le_bytes());
    out.extend_from_slice(&(unpacked.len() as u64).to_le_bytes());
    out.extend_from_slice(stream);
    out
}

/// `DXIL` part data: an empty program header, the `XDAT` index, then the
/// blobs as (kind, data).
pub fn dxil_part(blobs: &[(u32, &[u8])]) -> Vec<u8> {
    let mut out = vec![0u8; XDAT_OFFSET];
    out.extend_from_slice(b"XDAT");
    out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x24u32.to_le_bytes());
    let mut offset = out.len() + 12 * blobs.len();
    for (kind, blob) in blobs {
        for v in [*kind, offset as u32, blob.len() as u32] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        offset += blob.len();
    }
    for (_, blob) in blobs {
        out.extend_from_slice(blob);
    }
    out
}

/// Stage data with no semantics: zero counts (and, for a pixel program,
/// every render target's export patch absent).
fn empty_stage_data(stage: HwStage) -> Vec<u8> {
    match stage {
        HwStage::Ps => {
            let mut data = vec![0; PS_PATCHES + MRTS * PATCH_BYTES];
            for mrt in 0..MRTS {
                let at = PS_PATCHES + mrt * PATCH_BYTES;
                data[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            data
        }
        _ => vec![0; VS_OUTPUT_COUNT + 4],
    }
}

/// A one-program container: a `DXIL` part holding the program packed, and
/// a float4 signature element each way when given (name, DXIL semantic
/// kind: 3 position, 16 target).
fn container(
    stage: HwStage,
    vgprs: u32,
    words: &[u32],
    input: Option<(&str, u8)>,
    output: Option<(&str, u8)>,
) -> Vec<u8> {
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let stage_data = empty_stage_data(stage);
    let blob = fssm(&[Program {
        stage,
        wave32: stage == HwStage::Ps,
        regs: vgprs | 31 << 20,
        stage_data: &stage_data,
        code: &code,
    }]);
    let dxil = dxil_part(&[(0x0a, b"xbop"), (KIND_NATIVE, &xbch(&blob))]);
    let element = |(name, kind): (&str, u8)| SigElement {
        name: name.into(),
        index: 0,
        kind,
        comp: 9,
        interp: 0,
        row: Some(0),
        col: 0,
        cols: 4,
        usage: 0xf,
        half: false,
    };
    let isg1 = signature(&input.map(element).into_iter().collect::<Vec<_>>(), true);
    let osg1 = signature(&output.map(element).into_iter().collect::<Vec<_>>(), false);
    build(&[
        (FourCC::ISG1, &isg1),
        (FourCC::OSG1, &osg1),
        (FourCC::DXIL, &dxil),
    ])
}

const S_ENDPGM: u32 = 0xBF81_0000;
/// The literal operand (the next dword).
const LITERAL: u32 = 255;

/// VOP1 `<op> v<dst>, <src>` (`src` 256 + n is v<n>).
const fn vop1(op: u32, dst: u32, src: u32) -> u32 {
    0x7E00_0000 | dst << 17 | op << 9 | src
}

/// VOP2 `<op> v<dst>, <src0>, v<src1>`.
const fn vop2(op: u32, dst: u32, src0: u32, src1: u32) -> u32 {
    op << 25 | dst << 17 | src1 << 9 | src0
}

/// `exp <target> v<r>, v<r+1>, v<r+2>, v<r+3> done` (`vm` for a colour).
const fn exp(target: u32, r: u32, vm: bool) -> [u32; 2] {
    let vm = if vm { 1 << 12 } else { 0 };
    [
        0xF800_080F | vm | target << 4,
        r | (r + 1) << 8 | (r + 2) << 16 | (r + 3) << 24,
    ]
}

/// A vertex program that places a small triangle from the vertex id alone
/// (no vertex buffer: `SV_VertexID` in v0): vertex n at
/// (`size` * (n & 1), `size` * (n >> 1), 0, 1), so ids 0, 1, 2 make one
/// triangle with its right angle at the target's centre.
pub fn vertex_id_triangle(size: f32) -> Vec<u8> {
    let words = [
        vop2(0x1b, 1, 129, 0),     // v_and_b32 v1, 1, v0
        vop2(0x16, 2, 129, 0),     // v_lshrrev_b32 v2, 1, v0
        vop1(0x06, 1, 257),        // v_cvt_f32_u32 v1, v1
        vop1(0x06, 2, 258),        // v_cvt_f32_u32 v2, v2
        vop2(0x08, 1, LITERAL, 1), // v_mul_f32 v1, size, v1
        size.to_bits(),
        vop2(0x08, 2, LITERAL, 2), // v_mul_f32 v2, size, v2
        size.to_bits(),
        vop1(0x01, 3, 128),   // v_mov_b32 v3, 0
        vop1(0x01, 4, 242),   // v_mov_b32 v4, 1.0
        exp(12, 1, false)[0], // exp pos0 v1, v2, v3, v4 done
        exp(12, 1, false)[1],
        S_ENDPGM,
    ];
    container(HwStage::Vs, 8, &words, None, Some(("SV_Position", 3)))
}

/// A pixel program that writes `color` to render target 0.
pub fn constant_color(color: [f32; 4]) -> Vec<u8> {
    let mut words = Vec::new();
    for (r, c) in color.into_iter().enumerate() {
        words.extend([vop1(0x01, r as u32, LITERAL), c.to_bits()]); // v_mov_b32 v<r>, c
    }
    words.extend(exp(0, 0, true)); // exp mrt0 v0, v1, v2, v3 done vm
    words.push(S_ENDPGM);
    container(HwStage::Ps, 8, &words, None, Some(("SV_Target", 16)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::translate;

    /// The system values (D3D names: 1 position, 64 target) of a host
    /// container's `OSG1` rows.
    fn written(dxil: &[u8]) -> Vec<u32> {
        let c = crate::Container::parse(dxil).unwrap();
        let osg1 = c.part(FourCC::OSG1).expect("an OSG1 part").data;
        let count = crate::dxbc::u32_at(osg1, 0).unwrap() as usize;
        (0..count)
            .map(|i| crate::dxbc::u32_at(osg1, 8 + 32 * i + 12).unwrap())
            .collect()
    }

    /// What `translate` makes of the two (the vertex program writes the
    /// position, the pixel program render target 0), and the DXIL
    /// validator's verdict where there is one (Windows, `dxil.dll`).
    #[test]
    fn the_smoke_programs_translate() {
        for (stage, container, writes) in [
            (HwStage::Vs, vertex_id_triangle(0.1), 1),
            (HwStage::Ps, constant_color([1.0, 0.5, 0.0, 1.0]), 64),
        ] {
            let shader = translate(&container).unwrap_or_else(|e| panic!("{stage:?}: {e}"));
            assert_eq!(shader.stage, stage);
            assert!(shader.user_data.is_empty(), "{stage:?} reads user data");
            assert_eq!(written(&shader.dxil), [writes], "{stage:?}");
            #[cfg(windows)]
            if let Ok(mut v) = crate::emit::validator::Validator::load(
                crate::emit::validator::Validator::default_path().as_deref(),
            ) {
                v.validate(&mut shader.dxil.clone())
                    .unwrap_or_else(|e| panic!("{stage:?}: {e}"));
            }
        }
    }

    /// The one program of a container, and what its I/O raises to.
    fn raised(bytes: &[u8]) -> (crate::NativeProgram, crate::io::Io) {
        let c = crate::Container::parse(bytes).unwrap();
        let mut programs = crate::native_programs(c.part(FourCC::DXIL).unwrap().data).unwrap();
        assert_eq!(programs.len(), 1);
        let native = programs.remove(0);
        let p = crate::spill::build(&native).unwrap();
        let io = crate::io::raise(&p, &native).unwrap();
        (native, io)
    }

    /// The vertex program reads the vertex id and no attribute (no vertex
    /// buffer), and exports all four channels of the position; the pixel
    /// program all four of render target 0, its last export.
    #[test]
    fn the_programs_export_what_they_draw() {
        use crate::io::{Input, Target};
        let (vs, io) = raised(&vertex_id_triangle(0.1));
        assert_eq!((vs.stage, vs.wave64), (HwStage::Vs, true));
        let no_semantics = super::super::StageIo::Vs {
            inputs: vec![],
            outputs: vec![],
        };
        assert_eq!(vs.io, no_semantics);
        let loaded: Vec<_> = io.inputs.iter().map(|i| i.1).collect();
        assert!(loaded.contains(&Input::VertexId), "{loaded:?}");
        let attribute = |i: &Input| matches!(i, Input::Attribute { .. });
        assert!(!loaded.iter().any(attribute), "{loaded:?}");
        assert!(full(&io, Target::Position(0)), "{:?}", io.exports);
        let (ps, io) = raised(&constant_color([1.0, 0.5, 0.0, 1.0]));
        assert_eq!((ps.stage, ps.wave64), (HwStage::Ps, false));
        assert!(io.inputs.is_empty(), "{:?}", io.inputs);
        assert!(full(&io, Target::Color(0)), "{:?}", io.exports);
    }

    /// Whether the program's last export is to `target`, every channel.
    fn full(io: &crate::io::Io, target: crate::io::Target) -> bool {
        io.exports
            .iter()
            .any(|e| e.target == target && e.done && e.values.iter().all(Option::is_some))
    }
}
