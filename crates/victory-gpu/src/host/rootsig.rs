//! The root signatures of our own shaders. Each declares its own in HLSL
//! (`#define RS "..."` and `[RootSignature(RS)]`), which the system's
//! compiler writes into the bytecode as its `RTS0` part. Some compilers
//! write none: Wine's `d3dcompiler_47` ignores the attribute. Then the same
//! string is read here and serialized with `D3D12SerializeVersionedRootSignature`
//! (version 1.1, as the compiler's). Bytecode that has its part is used as is.
//!
//! The string is read from the first line of the source that starts with
//! `#define RS` (spaces or tabs between the words): it must be one quoted
//! string on that line, with no line continuation.

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
    let Some(blob) = fallback(code, source)? else {
        // SAFETY: the bytes live through the call.
        return unsafe { device.CreateRootSignature(0, code) };
    };
    // SAFETY: the blob's own bytes, alive through the call.
    unsafe {
        let bytes = parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        device.CreateRootSignature(0, bytes)
    }
}

/// The `count` items at `at`, or none when `at` is null or `count` is 0
/// (`from_raw_parts` refuses a null pointer, even for a count of 0).
///
/// # Safety
/// Unless null or with a count of 0, `at` points at `count` live items that
/// outlive `'a`.
unsafe fn parts<'a, T>(at: *const T, count: usize) -> &'a [T] {
    if at.is_null() || count == 0 {
        return &[];
    }
    // SAFETY: as the caller promises.
    unsafe { std::slice::from_raw_parts(at, count) }
}

/// The root signature serialized from `source`'s string when `code` has none
/// of its own; `None` when it has one.
fn fallback(code: &[u8], source: &str) -> Result<Option<ID3DBlob>> {
    if has_part(code, *b"RTS0") {
        return Ok(None);
    }
    serialize(defined(source).ok_or_else(|| unread("no #define RS"))?).map(Some)
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
    // Each part has its offset after the 32 bytes of the header.
    if count > code.len().saturating_sub(32) / 4 {
        return false;
    }
    (0..count).any(|i| word(32 + 4 * i).is_some_and(|at| code.get(at..at + 4) == Some(&fourcc[..])))
}

/// The string `source` defines as `RS`.
fn defined(source: &str) -> Option<&str> {
    // `word` and what follows it, if whitespace follows it.
    fn after<'a>(text: &'a str, word: &str) -> Option<&'a str> {
        let rest = text.strip_prefix(word)?;
        rest.starts_with(char::is_whitespace)
            .then(|| rest.trim_start())
    }
    let line = source
        .lines()
        .find_map(|l| after(after(l.trim_start(), "#define")?, "RS"))?;
    line.trim_end().strip_prefix('"')?.strip_suffix('"')
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
                parts(
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
    let mut chars = text.chars();
    let letter = chars.next().ok_or_else(|| unread(text))?;
    Ok((letter.to_ascii_lowercase(), number(chars.as_str())?))
}

/// The key of an argument (`register` for a plain one), refused if `seen`
/// already holds it.
fn once<'a>(seen: &mut Vec<&'a str>, argument: &'a str) -> Result<()> {
    let key = keyed(argument).map_or("register", |(k, _)| k);
    if seen.contains(&key) {
        return Err(unread(argument));
    }
    seen.push(key);
    Ok(())
}

/// Refuses `item` unless `seen` holds every key of `needed`.
fn needs(seen: &[&str], needed: &[&str], item: &str) -> Result<()> {
    match needed.iter().all(|n| seen.contains(n)) {
        true => Ok(()),
        false => Err(unread(item)),
    }
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
    let mut rooted = false;
    for item in split(text) {
        let (name, arguments) = call(item)?;
        let mut ranges = Vec::new();
        let parameter = match name {
            "RootFlags" => {
                let ([a], false) = (&arguments[..], rooted) else {
                    return Err(unread(item));
                };
                read.flags.0 = flags(a, ROOT_FLAGS)?;
                rooted = true;
                continue;
            }
            "StaticSampler" => {
                read.samplers.push(sampler(&arguments)?);
                continue;
            }
            "RootConstants" => constants(&arguments)?,
            "CBV" | "SRV" | "UAV" => descriptor(name, &arguments)?,
            "DescriptorTable" => {
                let mut shown = None;
                for a in &arguments {
                    match keyed(a) {
                        Some(("visibility", v)) if !a.contains('(') => {
                            if shown.replace(visibility(v)?).is_some() {
                                return Err(unread(a));
                            }
                        }
                        _ => ranges.push(range(a)?),
                    }
                }
                let shown = shown.unwrap_or(D3D12_SHADER_VISIBILITY_ALL);
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
    let mut seen = Vec::new();
    for a in arguments {
        once(&mut seen, a)?;
        match keyed(a) {
            Some(("num32BitConstants", v)) => c.Num32BitValues = number(v)?,
            Some(("space", v)) => c.RegisterSpace = number(v)?,
            Some(("visibility", v)) => shown = visibility(v)?,
            None if register(a)?.0 == 'b' => c.ShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    needs(&seen, &["register", "num32BitConstants"], "RootConstants")?;
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
    let mut seen = Vec::new();
    for a in arguments {
        once(&mut seen, a)?;
        match keyed(a) {
            Some(("space", v)) => d.RegisterSpace = number(v)?,
            Some(("visibility", v)) => shown = visibility(v)?,
            Some(("flags", v)) => d.Flags.0 = flags(v, DESCRIPTOR_FLAGS)?,
            None if register(a)?.0 == letter(kind) => d.ShaderRegister = register(a)?.1,
            _ => return Err(unread(a)),
        }
    }
    needs(&seen, &["register"], kind)?;
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
    let mut seen = Vec::new();
    for a in &arguments {
        once(&mut seen, a)?;
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
    needs(&seen, &["register"], item)?;
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
    let mut seen = Vec::new();
    for a in arguments {
        once(&mut seen, a)?;
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
    needs(&seen, &["register"], "StaticSampler")?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::Interface;

    /// The files whose shaders declare `#define RS`, as compiled in.
    const FILES: &[(&str, &str)] = &[
        ("ambient.rs", include_str!("../ambient.rs")),
        ("compose.rs", include_str!("../compose.rs")),
        ("host.rs", include_str!("../host.rs")),
        ("host/awake.rs", include_str!("awake.rs")),
        ("host/send.rs", include_str!("send.rs")),
        ("host/upcheck.rs", include_str!("upcheck.rs")),
        ("indirect.rs", include_str!("../indirect.rs")),
        ("indirect/batch.rs", include_str!("../indirect/batch.rs")),
        ("indirect/bench.rs", include_str!("../indirect/bench.rs")),
        ("inplace.rs", include_str!("../inplace.rs")),
        ("lightprobe.rs", include_str!("../lightprobe.rs")),
        (
            "replay/play/merge.rs",
            include_str!("../replay/play/merge.rs"),
        ),
        ("resource.rs", include_str!("../resource.rs")),
        ("tiles.rs", include_str!("../tiles.rs")),
    ];

    /// Every `#define RS` string of `text`, a Rust file: in raw strings as
    /// written, in plain ones unescaped. The template `{rs}` (lightprobe's,
    /// whose strings are its constants) is left out.
    fn strings_in(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for line in text.lines().filter(|l| l.contains("#define RS")) {
            let unescaped = line.replace("\\\"", "\"").replace("\\n", "\n");
            for part in unescaped.lines() {
                let Some(at) = part.find("#define RS") else {
                    continue;
                };
                let string = defined(&part[at..]).unwrap_or_else(|| panic!("unread: {line}"));
                if string != "{rs}" {
                    found.push(string.to_owned());
                }
            }
        }
        found
    }

    /// Every root signature string of the crate's shaders.
    fn our_strings() -> Vec<(&'static str, String)> {
        let mut all = Vec::new();
        for (file, text) in FILES {
            let found = strings_in(text);
            // lightprobe's strings are its constants, below.
            let templated = text.contains(r#"#define RS \"{rs}\""#);
            assert!(
                !found.is_empty() || templated,
                "{file} declares no root signature any more"
            );
            all.extend(found.into_iter().map(|s| (*file, s)));
        }
        for rs in [crate::lightprobe::RS_EXPOSURE, crate::lightprobe::RS_LUX] {
            all.push(("lightprobe.rs", rs.to_owned()));
        }
        all
    }

    /// `text` in a compute shader of nothing that declares it.
    fn shader(text: &str) -> String {
        format!(
            "#define RS \"{text}\"\n[RootSignature(RS)]\n[numthreads(1, 1, 1)]\nvoid main() {{}}\n"
        )
    }

    fn bytes(blob: &ID3DBlob) -> &[u8] {
        // SAFETY: the blob's own bytes, borrowed with it.
        unsafe { parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()) }
    }

    /// The default a range's flags of none stand for in version 1.1.
    fn range_flags(flags: i32, kind: D3D12_DESCRIPTOR_RANGE_TYPE) -> i32 {
        match (flags, kind) {
            (0, D3D12_DESCRIPTOR_RANGE_TYPE_UAV) => D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE.0,
            (0, D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER) => 0,
            (0, _) => D3D12_DESCRIPTOR_RANGE_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0,
            (f, _) => f,
        }
    }

    /// The default a root descriptor's flags of none stand for in 1.1.
    fn descriptor_flags(flags: i32, kind: D3D12_ROOT_PARAMETER_TYPE) -> i32 {
        match (flags, kind) {
            (0, D3D12_ROOT_PARAMETER_TYPE_UAV) => D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE.0,
            (0, _) => D3D12_ROOT_DESCRIPTOR_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE.0,
            (f, _) => f,
        }
    }

    fn sampler_words(s: &D3D12_STATIC_SAMPLER_DESC) -> String {
        format!(
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
        )
    }

    /// What a description of either version says, in words: version 1.0
    /// has no range or descriptor flags; in 1.1 a flag left at none is
    /// written as the default it stands for.
    fn words(desc: &D3D12_VERSIONED_ROOT_SIGNATURE_DESC) -> Vec<String> {
        let mut out = vec![format!("version {}", desc.Version.0)];
        // SAFETY: the deserializer's arrays, of the counts it gives, read
        // as the version it gives.
        unsafe {
            if desc.Version == D3D_ROOT_SIGNATURE_VERSION_1_0 {
                let d = &desc.Anonymous.Desc_1_0;
                out.push(format!("flags {}", d.Flags.0));
                for p in parts(d.pParameters, d.NumParameters as usize) {
                    let shown = p.ShaderVisibility.0;
                    match p.ParameterType {
                        D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE => {
                            let t = p.Anonymous.DescriptorTable;
                            out.push(format!("table {shown}"));
                            for r in parts(t.pDescriptorRanges, t.NumDescriptorRanges as usize) {
                                out.push(format!(
                                    "  range {} {} {} {} {}",
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
                            let r = p.Anonymous.Descriptor;
                            out.push(format!(
                                "descriptor {} {shown} {} {}",
                                kind.0, r.ShaderRegister, r.RegisterSpace
                            ));
                        }
                    }
                }
                for s in parts(d.pStaticSamplers, d.NumStaticSamplers as usize) {
                    out.push(sampler_words(s));
                }
                return out;
            }
            let d = &desc.Anonymous.Desc_1_1;
            out.push(format!("flags {}", d.Flags.0));
            for p in parts(d.pParameters, d.NumParameters as usize) {
                let shown = p.ShaderVisibility.0;
                match p.ParameterType {
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE => {
                        let t = p.Anonymous.DescriptorTable;
                        out.push(format!("table {shown}"));
                        for r in parts(t.pDescriptorRanges, t.NumDescriptorRanges as usize) {
                            out.push(format!(
                                "  range {} {} {} {} {} {}",
                                r.RangeType.0,
                                r.NumDescriptors,
                                r.BaseShaderRegister,
                                r.RegisterSpace,
                                r.OffsetInDescriptorsFromTableStart,
                                range_flags(r.Flags.0, r.RangeType)
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
                        let r = p.Anonymous.Descriptor;
                        out.push(format!(
                            "descriptor {} {shown} {} {} {}",
                            kind.0,
                            r.ShaderRegister,
                            r.RegisterSpace,
                            descriptor_flags(r.Flags.0, kind)
                        ));
                    }
                }
            }
            for s in parts(d.pStaticSamplers, d.NumStaticSamplers as usize) {
                out.push(sampler_words(s));
            }
        }
        out
    }

    fn deserializer(bytes: &[u8]) -> ID3D12VersionedRootSignatureDeserializer {
        let mut d: Option<ID3D12VersionedRootSignatureDeserializer> = None;
        // SAFETY: the bytes live through the call; the out pointer is ours.
        unsafe {
            D3D12CreateVersionedRootSignatureDeserializer(
                bytes.as_ptr().cast(),
                bytes.len(),
                &ID3D12VersionedRootSignatureDeserializer::IID,
                (&raw mut d).cast(),
            )
            .unwrap();
        }
        d.unwrap()
    }

    /// The compiler's root signature (`theirs`, as it wrote it) and ours at
    /// the version the compiler wrote, in words.
    fn compared(theirs: &[u8], ours: &[u8]) -> (Vec<String>, Vec<String>) {
        let (theirs, ours) = (deserializer(theirs), deserializer(ours));
        // SAFETY: the descriptions are the deserializers', read while they live.
        unsafe {
            let theirs = &*theirs.GetUnconvertedRootSignatureDesc();
            let at_theirs = &*ours.GetRootSignatureDescAtVersion(theirs.Version).unwrap();
            (words(theirs), words(at_theirs))
        }
    }

    /// Every string of our shaders is read, serialized, and taken by the
    /// device, both by itself and as the root signature of a shader compiled
    /// with it (from its `RTS0`, or else ours).
    #[test]
    fn every_string_of_ours_is_read() {
        use windows::core::s;
        let host = crate::host::host().expect("a device");
        let all = our_strings();
        for (file, text) in &all {
            let blob = serialize(text).unwrap_or_else(|e| panic!("{file}: {text}: {e}"));
            // SAFETY: the bytes live through the call.
            let made: Result<ID3D12RootSignature> =
                unsafe { host.device.CreateRootSignature(0, bytes(&blob)) };
            made.unwrap_or_else(|e| panic!("{file}: {text}: {e}"));
            let source = shader(text);
            assert_eq!(defined(&source), Some(text.as_str()));
            let code = crate::host::compile(&source, s!("main"), s!("cs_5_1")).unwrap();
            root_signature(&host.device, &code, &source)
                .unwrap_or_else(|e| panic!("{file}: {text}: {e}"));
        }
        eprintln!("{} root signature strings read", all.len());
        // Where the sources are at hand, no file is left out of FILES.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        let mut walked = false;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            walked = true;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if name != "host/rootsig.rs" && text.contains("#define RS") {
                    assert!(
                        FILES.iter().any(|(f, _)| *f == name),
                        "{name} is not in FILES"
                    );
                }
            }
        }
        if !walked {
            eprintln!("skipped source walk: {} not readable", root.display());
        }
    }

    /// Where the system's compiler writes its own root signature (`RTS0`),
    /// the fallback, given the same shader with that part stripped, makes
    /// the same one, and the stripped shader takes it.
    #[test]
    fn the_fallback_equals_the_compilers_own() {
        use windows::Win32::Graphics::Direct3D::Fxc::{
            D3DCOMPILER_STRIP_ROOT_SIGNATURE, D3DStripShader,
        };
        use windows::core::s;
        let host = crate::host::host().expect("a device");
        let all = our_strings();
        let (mut skipped, mut versions) = (0, std::collections::BTreeMap::new());
        for (file, text) in &all {
            let source = shader(text);
            let code = crate::host::compile(&source, s!("main"), s!("cs_5_1")).unwrap();
            if !has_part(&code, *b"RTS0") {
                skipped += 1;
                continue;
            }
            assert!(
                fallback(&code, &source).unwrap().is_none(),
                "{file}: {text}"
            );
            // SAFETY: the bytes live through the call.
            let stripped = unsafe {
                D3DStripShader(
                    code.as_ptr().cast(),
                    code.len(),
                    D3DCOMPILER_STRIP_ROOT_SIGNATURE.0 as u32,
                )
            }
            .unwrap();
            let stripped = bytes(&stripped);
            assert!(!has_part(stripped, *b"RTS0"), "{file}: {text}");
            let ours = fallback(stripped, &source).unwrap().expect("no RTS0 left");
            let (theirs, ours_words) = compared(&code, bytes(&ours));
            *versions.entry(theirs[0].clone()).or_insert(0) += 1;
            assert_eq!(theirs, ours_words, "{file}: {text}");
            let root = root_signature(&host.device, stripped, &source).unwrap();
            // SAFETY: the description points at live data.
            let pso: Result<ID3D12PipelineState> = unsafe {
                host.device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&root),
                        CS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: stripped.as_ptr().cast(),
                            BytecodeLength: stripped.len(),
                        },
                        ..Default::default()
                    })
            };
            pso.unwrap_or_else(|e| panic!("{file}: {text}: {e}"));
        }
        eprintln!(
            "compared {} of {} ({versions:?})",
            all.len() - skipped,
            all.len()
        );
        if skipped > 0 {
            eprintln!("skipped compare: no RTS0 ({skipped} of {})", all.len());
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
            "UAV()",
            "UAV(space=1)",
            "UAV(u0, u1)",
            "UAV(u0, space=1, space=2)",
            "UAV(\u{e9}0)",
            "RootConstants(b0)",
            "RootConstants(num32BitConstants=1)",
            "RootConstants(num32BitConstants=1, num32BitConstants=2, b0)",
            "DescriptorTable(SRV(numDescriptors=2))",
            "DescriptorTable(SRV(t0), visibility=SHADER_VISIBILITY_PIXEL, visibility=SHADER_VISIBILITY_ALL)",
            "StaticSampler(filter=FILTER_MIN_MAG_MIP_POINT)",
            "StaticSampler(s0, addressU=TEXTURE_ADDRESS_WRAP, addressU=TEXTURE_ADDRESS_CLAMP)",
            "RootFlags(0), RootFlags(0)",
            "RootFlags(0, 0)",
        ] {
            assert_eq!(parse(text).err().map(|e| e.code()), Some(UNREAD), "{text}");
        }
        assert!(defined("RWByteAddressBuffer b : register(u0);").is_none());
        assert!(defined("#define RSX \"UAV(u0)\"").is_none());
        assert_eq!(defined("  #define\tRS  \"UAV(u0)\" \n"), Some("UAV(u0)"));
        assert_eq!(defined("#define RS\t\"UAV(u0)\""), Some("UAV(u0)"));
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
        // A count past the container's end is not read.
        code[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!has_part(&code, *b"RTS0"));
    }
}
