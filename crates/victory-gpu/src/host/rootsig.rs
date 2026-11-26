//! The root signatures of our own shaders. Each declares its own in HLSL
//! (`#define RS "..."` and `[RootSignature(RS)]`), which the system's
//! compiler writes into the bytecode as its `RTS0` part. Some compilers
//! write none: Wine's `d3dcompiler_47` ignores the attribute. Then the same
//! string is read here and serialized with `D3D12SerializeVersionedRootSignature`
//! (version 1.1, as the compiler's). Bytecode that has its part is used as is.

use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::{Error, HRESULT, Result};

/// `E_INVALIDARG`: a root signature string we do not read.
const UNREAD: HRESULT = HRESULT(0x8007_0057_u32 as i32);

/// The root signature of `code`, compiled from `source`: the one inside the
/// bytecode if it has one, else the one `source` defines as `RS`.
pub(crate) fn root_signature(
    device: &ID3D12Device,
    code: &[u8],
    source: &str,
) -> Result<ID3D12RootSignature> {
    if has_part(code, *b"RTS0") {
        // SAFETY: the bytes live through the call.
        return unsafe { device.CreateRootSignature(0, code) };
    }
    let blob = serialize(defined(source).ok_or_else(|| unread("no #define RS"))?)?;
    // SAFETY: the blob's own bytes, alive through the call.
    unsafe {
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        device.CreateRootSignature(0, bytes)
    }
}

/// Whether the DXBC container `code` has a part named `fourcc`.
fn has_part(code: &[u8], fourcc: [u8; 4]) -> bool {
    let word = |at: usize| {
        code.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if code.get(..4) != Some(b"DXBC") {
        return false;
    }
    let Some(count) = word(28) else { return false };
    (0..count).any(|i| word(32 + 4 * i).is_some_and(|at| code.get(at..at + 4) == Some(&fourcc[..])))
}

/// The string `source` defines as `RS`.
fn defined(source: &str) -> Option<&str> {
    let line = source
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("#define RS "))?;
    line.trim().strip_prefix('"')?.strip_suffix('"')
}

fn unread(what: &str) -> Error {
    Error::new(UNREAD, format!("root signature string: {what}"))
}

/// `text` serialized as a version 1.1 root signature.
pub(crate) fn serialize(text: &str) -> Result<ID3DBlob> {
    let read = parse(text)?;
    let mut parameters = read.parameters;
    for (parameter, ranges) in parameters.iter_mut().zip(&read.ranges) {
        if parameter.ParameterType == D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE {
            parameter.Anonymous.DescriptorTable = D3D12_ROOT_DESCRIPTOR_TABLE1 {
                NumDescriptorRanges: ranges.len() as u32,
                pDescriptorRanges: ranges.as_ptr(),
            };
        }
    }
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: parameters.len() as u32,
                pParameters: parameters.as_ptr(),
                NumStaticSamplers: read.samplers.len() as u32,
                pStaticSamplers: read.samplers.as_ptr(),
                Flags: read.flags,
            },
        },
    };
    let mut blob = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the description and what it points at live through the call.
    let serialized =
        unsafe { D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut errors)) };
    if let Err(e) = serialized {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(
                    errors.GetBufferPointer().cast::<u8>(),
                    errors.GetBufferSize(),
                )
            };
            tracing::warn!(
                "a root signature of ours: {}",
                String::from_utf8_lossy(text)
            );
        }
        return Err(e);
    }
    blob.ok_or_else(Error::empty)
}

/// A root signature string, read. A table's ranges are in `ranges` at the
/// table's index (empty for the other parameters).
struct Read {
    parameters: Vec<D3D12_ROOT_PARAMETER1>,
    ranges: Vec<Vec<D3D12_DESCRIPTOR_RANGE1>>,
    samplers: Vec<D3D12_STATIC_SAMPLER_DESC>,
    flags: D3D12_ROOT_SIGNATURE_FLAGS,
}

/// `text` split at its commas outside parentheses, each piece trimmed.
fn split(text: &str) -> Vec<&str> {
    let (mut pieces, mut depth, mut from) = (Vec::new(), 0, 0);
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                pieces.push(text[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    pieces.push(text[from..].trim());
    pieces.retain(|p| !p.is_empty());
    pieces
}

/// `Name(arguments)` as its name and its arguments.
fn call(item: &str) -> Result<(&str, Vec<&str>)> {
    let (name, rest) = item.split_once('(').ok_or_else(|| unread(item))?;
    let inner = rest
        .trim_end()
        .strip_suffix(')')
        .ok_or_else(|| unread(item))?;
    Ok((name.trim(), split(inner)))
}

/// An argument as `key = value`, or `None` for a plain one.
fn keyed(argument: &str) -> Option<(&str, &str)> {
    argument.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
}

fn number(text: &str) -> Result<u32> {
    text.parse().map_err(|_| unread(text))
}

/// A register such as `t3`: its letter and number.
fn register(text: &str) -> Result<(char, u32)> {
    let letter = text.chars().next().ok_or_else(|| unread(text))?;
    Ok((letter.to_ascii_lowercase(), number(&text[1..])?))
}

/// `A|B|C`, each name looked up in `names`; `0` is none.
fn flags(text: &str, names: &[(&str, i32)]) -> Result<i32> {
    text.split('|').map(str::trim).try_fold(0, |all, name| {
        if name == "0" {
            return Ok(all);
        }
        let (_, bits) = names
            .iter()
            .find(|(n, _)| *n == name)
            .ok_or_else(|| unread(name))?;
        Ok(all | bits)
    })
}

fn visibility(text: &str) -> Result<D3D12_SHADER_VISIBILITY> {
    Ok(match text {
        "SHADER_VISIBILITY_ALL" => D3D12_SHADER_VISIBILITY_ALL,
        "SHADER_VISIBILITY_VERTEX" => D3D12_SHADER_VISIBILITY_VERTEX,
        "SHADER_VISIBILITY_HULL" => D3D12_SHADER_VISIBILITY_HULL,
        "SHADER_VISIBILITY_DOMAIN" => D3D12_SHADER_VISIBILITY_DOMAIN,
        "SHADER_VISIBILITY_GEOMETRY" => D3D12_SHADER_VISIBILITY_GEOMETRY,
        "SHADER_VISIBILITY_PIXEL" => D3D12_SHADER_VISIBILITY_PIXEL,
        "SHADER_VISIBILITY_AMPLIFICATION" => D3D12_SHADER_VISIBILITY_AMPLIFICATION,
        "SHADER_VISIBILITY_MESH" => D3D12_SHADER_VISIBILITY_MESH,
        _ => return Err(unread(text)),
    })
}

const ROOT_FLAGS: &[(&str, i32)] = &[
    (
        "ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT",
        D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT.0,
    ),
    (
        "DENY_VERTEX_SHADER_ROOT_ACCESS",
        D3D12_ROOT_SIGNATURE_FLAG_DENY_VERTEX_SHADER_ROOT_ACCESS.0,
    ),
    (
        "DENY_HULL_SHADER_ROOT_ACCESS",
        D3D12_ROOT_SIGNATURE_FLAG_DENY_HULL_SHADER_ROOT_ACCESS.0,
    ),
    (
        "DENY_DOMAIN_SHADER_ROOT_ACCESS",
        D3D12_ROOT_SIGNATURE_FLAG_DENY_DOMAIN_SHADER_ROOT_ACCESS.0,
    ),
    (
        "DENY_GEOMETRY_SHADER_ROOT_ACCESS",
        D3D12_ROOT_SIGNATURE_FLAG_DENY_GEOMETRY_SHADER_ROOT_ACCESS.0,
    ),
    (
        "DENY_PIXEL_SHADER_ROOT_ACCESS",
        D3D12_ROOT_SIGNATURE_FLAG_DENY_PIXEL_SHADER_ROOT_ACCESS.0,
    ),
    (
        "ALLOW_STREAM_OUTPUT",
        D3D12_ROOT_SIGNATURE_FLAG_ALLOW_STREAM_OUTPUT.0,
    ),
];

const DESCRIPTOR_FLAGS: &[(&str, i32)] = &[
    ("DATA_VOLATILE", D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE.0),
    (
        "DATA_STATIC_WHILE_SET_AT_EXECUTE",
        D3D12_ROOT_DESCRIPTOR_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0,
    ),
    ("DATA_STATIC", D3D12_ROOT_DESCRIPTOR_FLAG_DATA_STATIC.0),
];

const RANGE_FLAGS: &[(&str, i32)] = &[
    (
        "DESCRIPTORS_VOLATILE",
        D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE.0,
    ),
    ("DATA_VOLATILE", D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE.0),
    (
        "DATA_STATIC_WHILE_SET_AT_EXECUTE",
        D3D12_DESCRIPTOR_RANGE_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0,
    ),
    ("DATA_STATIC", D3D12_DESCRIPTOR_RANGE_FLAG_DATA_STATIC.0),
    (
        "DESCRIPTORS_STATIC_KEEPING_BUFFER_BOUNDS_CHECKS",
        D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_STATIC_KEEPING_BUFFER_BOUNDS_CHECKS.0,
    ),
];

fn parse(text: &str) -> Result<Read> {
    let mut read = Read {
        parameters: Vec::new(),
        ranges: Vec::new(),
        samplers: Vec::new(),
        flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    for item in split(text) {
        let (name, arguments) = call(item)?;
        let mut ranges = Vec::new();
        let parameter = match name {
            "RootFlags" => {
                for a in &arguments {
                    read.flags.0 |= flags(a, ROOT_FLAGS)?;
                }
                continue;
            }
            "StaticSampler" => {
                read.samplers.push(sampler(&arguments)?);
                continue;
            }
            "RootConstants" => constants(&arguments)?,
            "CBV" | "SRV" | "UAV" => descriptor(name, &arguments)?,
            "DescriptorTable" => {
                let mut shown = D3D12_SHADER_VISIBILITY_ALL;
                for a in &arguments {
                    match keyed(a) {
                        Some(("visibility", v)) if !a.contains('(') => shown = visibility(v)?,
                        _ => ranges.push(range(a)?),
                    }
                }
                D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    ShaderVisibility: shown,
                    ..Default::default()
                }
            }
            _ => return Err(unread(name)),
        };
        read.parameters.push(parameter);
        read.ranges.push(ranges);
    }
    Ok(read)
}

fn constants(arguments: &[&str]) -> Result<D3D12_ROOT_PARAMETER1> {
    let mut c = D3D12_ROOT_CONSTANTS::default();
    let mut shown = D3D12_SHADER_VISIBILITY_ALL;
    for a in arguments {
        match keyed(a) {
            Some(("num32BitConstants", v)) => c.Num32BitValues = number(v)?,
            Some(("space", v)) => c.RegisterSpace = number(v)?,
            Some(("visibility", v)) => shown = visibility(v)?,
            None if register(a)?.0 == 'b' => c.ShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    Ok(D3D12_ROOT_PARAMETER1 {
        ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
        Anonymous: D3D12_ROOT_PARAMETER1_0 { Constants: c },
        ShaderVisibility: shown,
    })
}

/// The register letter of a descriptor kind.
fn letter(kind: &str) -> char {
    match kind {
        "CBV" => 'b',
        "SRV" => 't',
        "UAV" => 'u',
        _ => 's',
    }
}

fn descriptor(kind: &str, arguments: &[&str]) -> Result<D3D12_ROOT_PARAMETER1> {
    let mut d = D3D12_ROOT_DESCRIPTOR1::default();
    let mut shown = D3D12_SHADER_VISIBILITY_ALL;
    for a in arguments {
        match keyed(a) {
            Some(("space", v)) => d.RegisterSpace = number(v)?,
            Some(("visibility", v)) => shown = visibility(v)?,
            Some(("flags", v)) => d.Flags.0 = flags(v, DESCRIPTOR_FLAGS)?,
            None if register(a)?.0 == letter(kind) => d.ShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    let kind = match kind {
        "CBV" => D3D12_ROOT_PARAMETER_TYPE_CBV,
        "SRV" => D3D12_ROOT_PARAMETER_TYPE_SRV,
        _ => D3D12_ROOT_PARAMETER_TYPE_UAV,
    };
    Ok(D3D12_ROOT_PARAMETER1 {
        ParameterType: kind,
        Anonymous: D3D12_ROOT_PARAMETER1_0 { Descriptor: d },
        ShaderVisibility: shown,
    })
}

fn range(item: &str) -> Result<D3D12_DESCRIPTOR_RANGE1> {
    let (kind, arguments) = call(item)?;
    let mut r = D3D12_DESCRIPTOR_RANGE1 {
        RangeType: match kind {
            "CBV" => D3D12_DESCRIPTOR_RANGE_TYPE_CBV,
            "SRV" => D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            "UAV" => D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            "Sampler" => D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
            _ => return Err(unread(kind)),
        },
        NumDescriptors: 1,
        OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
        ..Default::default()
    };
    for a in &arguments {
        match keyed(a) {
            Some(("numDescriptors", "unbounded")) => r.NumDescriptors = u32::MAX,
            Some(("numDescriptors", v)) => r.NumDescriptors = number(v)?,
            Some(("space", v)) => r.RegisterSpace = number(v)?,
            Some(("offset", "DESCRIPTOR_RANGE_OFFSET_APPEND")) => {}
            Some(("offset", v)) => r.OffsetInDescriptorsFromTableStart = number(v)?,
            Some(("flags", v)) => r.Flags.0 = flags(v, RANGE_FLAGS)?,
            None if register(a)?.0 == letter(kind) => r.BaseShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    Ok(r)
}

fn sampler(arguments: &[&str]) -> Result<D3D12_STATIC_SAMPLER_DESC> {
    // The defaults of the HLSL root signature language.
    let mut s = D3D12_STATIC_SAMPLER_DESC {
        Filter: D3D12_FILTER_ANISOTROPIC,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        MipLODBias: 0.0,
        MaxAnisotropy: 16,
        ComparisonFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL,
        BorderColor: D3D12_STATIC_BORDER_COLOR_OPAQUE_WHITE,
        MinLOD: 0.0,
        MaxLOD: D3D12_FLOAT32_MAX,
        ShaderRegister: 0,
        RegisterSpace: 0,
        ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
    };
    let address = |v: &str| {
        Ok(match v {
            "TEXTURE_ADDRESS_WRAP" => D3D12_TEXTURE_ADDRESS_MODE_WRAP,
            "TEXTURE_ADDRESS_MIRROR" => D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
            "TEXTURE_ADDRESS_CLAMP" => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            "TEXTURE_ADDRESS_BORDER" => D3D12_TEXTURE_ADDRESS_MODE_BORDER,
            "TEXTURE_ADDRESS_MIRROR_ONCE" => D3D12_TEXTURE_ADDRESS_MODE_MIRROR_ONCE,
            _ => return Err(unread(v)),
        })
    };
    for a in arguments {
        match keyed(a) {
            Some(("filter", v)) => {
                s.Filter = match v {
                    "FILTER_MIN_MAG_MIP_POINT" => D3D12_FILTER_MIN_MAG_MIP_POINT,
                    "FILTER_MIN_MAG_MIP_LINEAR" => D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                    "FILTER_MIN_MAG_LINEAR_MIP_POINT" => D3D12_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                    "FILTER_ANISOTROPIC" => D3D12_FILTER_ANISOTROPIC,
                    _ => return Err(unread(v)),
                }
            }
            Some(("addressU", v)) => s.AddressU = address(v)?,
            Some(("addressV", v)) => s.AddressV = address(v)?,
            Some(("addressW", v)) => s.AddressW = address(v)?,
            Some(("maxAnisotropy", v)) => s.MaxAnisotropy = number(v)?,
            Some(("space", v)) => s.RegisterSpace = number(v)?,
            Some(("visibility", v)) => s.ShaderVisibility = visibility(v)?,
            None if register(a)?.0 == 's' => s.ShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::Interface;

    /// Every kind of piece our shaders' strings hold.
    const STRINGS: &[&str] = &[
        "RootFlags(0)",
        "UAV(u0)",
        "RootConstants(num32BitConstants = 1, b0)",
        "RootConstants(num32BitConstants=6, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)",
        "DescriptorTable(SRV(t0), visibility = SHADER_VISIBILITY_PIXEL)",
        "RootConstants(num32BitConstants=15, b0), DescriptorTable(SRV(t0)), DescriptorTable(UAV(u0)), UAV(u1), UAV(u2), UAV(u3)",
        "DescriptorTable(SRV(t0, numDescriptors=2), visibility=SHADER_VISIBILITY_PIXEL), StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP)",
        "RootConstants(num32BitConstants=4,b0), DescriptorTable(SRV(t0,numDescriptors=8)), UAV(u0), DescriptorTable(CBV(b1,numDescriptors=8))",
        "RootConstants(num32BitConstants=4,b0), DescriptorTable(SRV(t0,numDescriptors=4),CBV(b1)), UAV(u0), StaticSampler(s0,filter=FILTER_MIN_MAG_MIP_LINEAR,addressU=TEXTURE_ADDRESS_WRAP,addressV=TEXTURE_ADDRESS_WRAP)",
        "DescriptorTable(SRV(t0,numDescriptors=64,flags=DESCRIPTORS_VOLATILE|DATA_VOLATILE)), UAV(u0)",
        "DescriptorTable(SRV(t0, numDescriptors = 2)), UAV(u0), StaticSampler(s0, filter = FILTER_MIN_MAG_MIP_POINT)",
    ];

    /// What a deserialized 1.1 description says, in words; a flag left at
    /// none is written as the default it stands for.
    fn words(desc: &D3D12_ROOT_SIGNATURE_DESC1) -> Vec<String> {
        let mut out = vec![format!("flags {}", desc.Flags.0)];
        // SAFETY: the deserializer's arrays, of the counts it gives.
        unsafe {
            for p in std::slice::from_raw_parts(desc.pParameters, desc.NumParameters as usize) {
                let shown = p.ShaderVisibility.0;
                match p.ParameterType {
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE => {
                        let t = p.Anonymous.DescriptorTable;
                        out.push(format!("table {shown}"));
                        for r in std::slice::from_raw_parts(
                            t.pDescriptorRanges,
                            t.NumDescriptorRanges as usize,
                        ) {
                            let flags = match (r.Flags.0, r.RangeType) {
                                (0, D3D12_DESCRIPTOR_RANGE_TYPE_UAV) => {
                                    D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE.0
                                }
                                (0, D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER) => 0,
                                (0, _) => {
                                    D3D12_DESCRIPTOR_RANGE_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0
                                }
                                (f, _) => f,
                            };
                            out.push(format!(
                                "  range {} {} {} {} {} {flags}",
                                r.RangeType.0,
                                r.NumDescriptors,
                                r.BaseShaderRegister,
                                r.RegisterSpace,
                                r.OffsetInDescriptorsFromTableStart
                            ));
                        }
                    }
                    D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS => {
                        let c = p.Anonymous.Constants;
                        out.push(format!(
                            "constants {shown} {} {} {}",
                            c.ShaderRegister, c.RegisterSpace, c.Num32BitValues
                        ));
                    }
                    kind => {
                        let d = p.Anonymous.Descriptor;
                        let flags = match (d.Flags.0, kind) {
                            (0, D3D12_ROOT_PARAMETER_TYPE_UAV) => {
                                D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE.0
                            }
                            (0, _) => D3D12_ROOT_DESCRIPTOR_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0,
                            (f, _) => f,
                        };
                        out.push(format!(
                            "descriptor {} {shown} {} {} {flags}",
                            kind.0, d.ShaderRegister, d.RegisterSpace
                        ));
                    }
                }
            }
            for s in
                std::slice::from_raw_parts(desc.pStaticSamplers, desc.NumStaticSamplers as usize)
            {
                out.push(format!(
                    "sampler {} {} {} {} {} {} {} {} {} {} {} {} {}",
                    s.Filter.0,
                    s.AddressU.0,
                    s.AddressV.0,
                    s.AddressW.0,
                    s.MipLODBias,
                    s.MaxAnisotropy,
                    s.ComparisonFunc.0,
                    s.BorderColor.0,
                    s.MinLOD,
                    s.MaxLOD,
                    s.ShaderRegister,
                    s.RegisterSpace,
                    s.ShaderVisibility.0
                ));
            }
        }
        out
    }

    /// `bytes`' root signature, deserialized as version 1.1, in words.
    fn deserialized(bytes: &[u8]) -> Vec<String> {
        // SAFETY: the bytes live through the calls; the description is the
        // deserializer's, read while it lives.
        unsafe {
            let mut d: Option<ID3D12VersionedRootSignatureDeserializer> = None;
            D3D12CreateVersionedRootSignatureDeserializer(
                bytes.as_ptr().cast(),
                bytes.len(),
                &ID3D12VersionedRootSignatureDeserializer::IID,
                (&raw mut d).cast(),
            )
            .unwrap();
            let d = d.unwrap();
            let desc = &*d
                .GetRootSignatureDescAtVersion(D3D_ROOT_SIGNATURE_VERSION_1_1)
                .unwrap();
            words(&desc.Anonymous.Desc_1_1)
        }
    }

    /// Each string serializes, gives a root signature the device takes, and,
    /// where the system's compiler writes its own (`RTS0`), the same one.
    #[test]
    fn our_strings_serialize_as_the_compiler_writes_them() {
        use windows::core::s;
        let host = crate::host::host().expect("a device");
        for text in STRINGS {
            let blob = serialize(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            // SAFETY: the blob's own bytes.
            let ours = unsafe {
                std::slice::from_raw_parts(
                    blob.GetBufferPointer().cast::<u8>(),
                    blob.GetBufferSize(),
                )
            };
            // SAFETY: the bytes live through the call.
            let made: Result<ID3D12RootSignature> =
                unsafe { host.device.CreateRootSignature(0, ours) };
            made.unwrap_or_else(|e| panic!("{text}: {e}"));
            let source = format!(
                "#define RS \"{text}\"\n[RootSignature(RS)]\n[numthreads(1, 1, 1)]\nvoid main() {{}}\n"
            );
            assert_eq!(defined(&source), Some(*text));
            let code = crate::host::compile(&source, s!("main"), s!("cs_5_1")).unwrap();
            root_signature(&host.device, &code, &source).unwrap_or_else(|e| panic!("{text}: {e}"));
            if has_part(&code, *b"RTS0") {
                assert_eq!(deserialized(ours), deserialized(&code), "{text}");
            }
        }
    }

    /// A string with a piece we do not read is refused, not half read.
    #[test]
    fn unknown_pieces_are_refused() {
        for text in [
            "Bogus(0)",
            "UAV(t0)",
            "RootConstants(num32BitConstants=1, b0, colour=1)",
            "DescriptorTable(SRV(t0, flags=SOMETIMES))",
            "StaticSampler(s0, filter=FILTER_NONE_AT_ALL)",
            "RootFlags(ALLOW_NOTHING)",
            "UAV(u0",
        ] {
            assert_eq!(parse(text).err().map(|e| e.code()), Some(UNREAD), "{text}");
        }
        assert!(defined("RWByteAddressBuffer b : register(u0);").is_none());
    }

    /// The part of a container is found only there.
    #[test]
    fn a_container_part_is_found_by_its_name() {
        let mut code = b"DXBC".to_vec();
        code.resize(28, 0);
        code.extend(1u32.to_le_bytes());
        code.extend(36u32.to_le_bytes());
        code.extend(b"RTS0");
        assert!(has_part(&code, *b"RTS0"));
        assert!(!has_part(&code, *b"SHDR"));
        assert!(!has_part(&code[..30], *b"RTS0"));
        assert!(!has_part(b"DXIL", *b"RTS0"));
    }
}
