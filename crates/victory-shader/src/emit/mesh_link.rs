//! Mesh and pixel shaders translated as a pair (D3DMetal): the mesh shader
//! again with only the outputs its pixel shader reads, ordered by size, so
//! that Metal's aligned layout of them fits its 32 KiB.
//!
//! A mesh shader whose outputs pass 32 KiB by Metal's layout
//! (`container::metal_mesh_output_bytes`) makes a D3DMetal pipeline that
//! hangs the queue (docs/macos.md). Many of the title's write attributes
//! their pixel shader never reads. [`translate_linked`] translates the pixel
//! shader, takes the arbitrary inputs it reads from its `ISG1`, translates
//! the mesh shader again keeping only those outputs (and those channels),
//! ordered by size (4, 2, 1 then 3 components) so the aligned layout has
//! no holes, then the pixel shader again with the same inputs. System
//! values stay on both sides. Both sides get their registers from one
//! [`layout`] (vertex outputs, then primitive outputs, then what the pixel
//! shader alone has): D3D12 matches the two by register. On D3DMetal with linking on ([`set_host_mesh_link`]) a mesh
//! shader over the limit is no longer refused when it is translated alone:
//! the pipeline tries the pair and refuses only if that fails
//! (`victory-gpu`, `pipeline/link.rs`).
//!
//! Ported from pragyaangaur/VIctory-macOS PR #161 (linking, pruning),
//! #401 (linking on Metal's aligned count, ordering by size)
//! and #282 (on by default under D3DMetal).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::container::SigElement;
use super::{Container, FourCC, HostShader, HwStage, ShaderError};

/// Arbitrary (`kind == 0`) pixel inputs read, by `(UPPERCASE_NAME, index)`:
/// the channels read, relative to the element's first channel (bit 0 is
/// its first component, wherever its register puts it).
pub(crate) type KeptOutputs = HashMap<(String, u32), u8>;

static HOST_MESH_LINK: AtomicBool = AtomicBool::new(false);

/// Tells the translator that pipelines link mesh shaders over Metal's limit
/// with their pixel shaders (D3DMetal with `VICTORY_LINK_MESH_OUTPUTS`
/// not `0`): such a mesh shader translated alone is not refused for its
/// size, its pipeline decides. Part of the translation fingerprint.
pub fn set_host_mesh_link(on: bool) {
    HOST_MESH_LINK.store(on, Ordering::Relaxed);
}

#[cfg(test)]
thread_local! {
    /// A test's choice for [`host_mesh_link`], for its thread only.
    pub(super) static TEST_MESH_LINK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// See [`set_host_mesh_link`].
pub fn host_mesh_link() -> bool {
    #[cfg(test)]
    if let Some(on) = TEST_MESH_LINK.get() {
        return on;
    }
    HOST_MESH_LINK.load(Ordering::Relaxed)
}

/// `VICTORY_MESH_OVER_LIMIT=1`: on D3DMetal, mesh shaders over Metal's
/// 32 KiB are translated, and their pipelines made, anyway.
pub fn mesh_over_limit_allowed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_MESH_OVER_LIMIT").is_some_and(|v| v != "0"))
}

/// A signature element's key across the two stages: D3D12 matches them
/// by semantic, at the same register and channels.
pub(crate) type Key = (String, u32);

fn key(e: &SigElement) -> Key {
    (e.name.to_ascii_uppercase(), e.index)
}

/// Where the elements of a linked pair go, the same on both sides: by
/// [`Key`], the register, first channel and interpolation mode. The pixel
/// shader's inputs are the mesh shader's vertex outputs, then its
/// primitive outputs from register `prim_base` on (the mesh shader's
/// `PSG1` counts its registers from 0).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    pub at: HashMap<Key, (u32, u8, u8)>,
    pub prim_base: u32,
}

/// What a linked translation on this thread keeps and where it puts it.
#[derive(Debug, Clone, Default)]
pub(super) struct Linked {
    pub keep: KeptOutputs,
    pub layout: Layout,
}

thread_local! {
    /// The linked translation under way ([`with_linked`]).
    static LINKED: RefCell<Option<Linked>> = const { RefCell::new(None) };
    /// A pixel shader's input interpolation modes, while noted
    /// ([`noting_pixel_modes`]).
    static MODES: RefCell<Option<HashMap<Key, u8>>> = const { RefCell::new(None) };
}

/// Puts a thread-local back as it was, also when the translation panics.
struct Restore<T: 'static> {
    cell: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    before: Option<T>,
}

impl<T: 'static> Drop for Restore<T> {
    fn drop(&mut self) {
        self.cell.set(self.before.take());
    }
}

fn set<T: 'static>(
    cell: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    value: Option<T>,
) -> Restore<T> {
    Restore {
        cell,
        before: cell.replace(value),
    }
}

/// Runs `translate` as one half of a linked pair: the mesh outputs and
/// pixel inputs cut down to `linked.keep` (`Lower::new`) and laid out as
/// `linked.layout` says (`Shader::finish`).
pub(super) fn with_linked<R>(linked: Linked, translate: impl FnOnce() -> R) -> R {
    let _restore = set(&LINKED, Some(linked));
    translate()
}

/// The outputs a linked translation on this thread keeps, if it is one.
pub(super) fn kept_outputs() -> Option<KeptOutputs> {
    LINKED.with_borrow(|l| l.as_ref().map(|l| l.keep.clone()))
}

/// Runs `translate` (a pixel shader's) noting its inputs' interpolation
/// modes as `Shader::finish` settles them.
fn noting_pixel_modes<R>(translate: impl FnOnce() -> R) -> (R, HashMap<Key, u8>) {
    let restore = set(&MODES, Some(HashMap::new()));
    let result = translate();
    let modes = MODES.take().unwrap_or_default();
    drop(restore);
    (result, modes)
}

/// `Shader::finish` of a pixel shader not linked: its inputs' modes, when
/// noted.
pub(super) fn note_pixel_inputs(inputs: &[SigElement]) {
    MODES.with_borrow_mut(|m| {
        if let Some(m) = m {
            m.clear();
            m.extend(inputs.iter().map(|e| (key(e), e.interp)));
        }
    });
}

/// Keeps the arbitrary elements with a channel in `keep`, trimmed to the
/// channels from the first read to the last, and orders them by size
/// (4, 2, 1 then 3 components) after the system values, which stay as
/// they are.
pub(super) fn prune_signature(elements: &mut Vec<SigElement>, keep: &KeptOutputs) {
    elements.retain_mut(|e| {
        if e.kind != 0 {
            return true;
        }
        if e.row.is_none() || e.cols == 0 {
            return false;
        }
        let used = keep
            .get(&(e.name.to_ascii_uppercase(), e.index))
            .copied()
            .unwrap_or(0);
        let rel = used & ((1u8 << e.cols) - 1);
        if rel == 0 {
            return false;
        }
        let first = rel.trailing_zeros() as u8;
        let last = 7 - rel.leading_zeros() as u8;
        e.col += first;
        e.cols = last - first + 1;
        true
    });
    elements.sort_by_key(|e| {
        if e.kind != 0 {
            (0u8, 0u8, String::new(), 0u32, 0u8)
        } else {
            let rank = match e.cols {
                4 => 0,
                2 => 1,
                1 => 2,
                _ => 3,
            };
            (1, rank, e.name.to_ascii_uppercase(), e.index, e.col)
        }
    });
}

/// The layout of a linked pair from the mesh shader's kept vertex and
/// primitive outputs, in [`prune_signature`]'s order: system values a
/// register each, then the arbitrary elements packed in order into
/// registers of four channels where the pixel shader's mode (`modes`;
/// the mesh shader's without one) and float or integer agree; the same for
/// the primitive outputs, from the register after the vertex outputs.
pub(super) fn layout(
    vertex: &[SigElement],
    prims: &[SigElement],
    modes: &HashMap<Key, u8>,
) -> Layout {
    let mut at = HashMap::new();
    let mut next = 0u32;
    let mut place = |els: &[SigElement], next: &mut u32| {
        // (register, channels used, mode, float)
        let mut open: Option<(u32, u8, u8, bool)> = None;
        for e in els.iter().filter(|e| e.row.is_some()) {
            let mode = modes.get(&key(e)).copied().unwrap_or(e.interp);
            if e.kind != 0 {
                at.insert(key(e), (*next, 0, mode));
                *next += 1;
                continue;
            }
            let float = e.comp == 9;
            match open {
                Some((r, used, m, f)) if used + e.cols <= 4 && m == mode && f == float => {
                    at.insert(key(e), (r, used, mode));
                    open = (used + e.cols < 4).then_some((r, used + e.cols, m, f));
                }
                _ => {
                    at.insert(key(e), (*next, 0, mode));
                    open = (e.cols < 4).then_some((*next, e.cols, mode, float));
                    *next += 1;
                }
            }
        }
    };
    place(vertex, &mut next);
    let prim_base = next;
    place(prims, &mut next);
    Layout { at, prim_base }
}

/// Gives `elements` their registers and channels by `layout`, less `base`
/// (a mesh shader's primitive outputs), once the lowering no longer looks
/// them up by the console's registers (DXIL names an element by its
/// position, a channel relative to its first). Arbitrary elements take the
/// pair's mode, one per register. Elements the layout does not hold (a
/// pixel shader's front face, sample index...) get a register each after
/// all of the layout's.
fn apply(elements: &mut [SigElement], layout: &Layout, base: u32) {
    let mut next = layout.at.values().map(|a| a.0 + 1).max().unwrap_or(0);
    for e in elements.iter_mut().filter(|e| e.row.is_some()) {
        match layout.at.get(&key(e)) {
            Some(&(row, col, mode)) if row >= base => {
                e.row = Some(row - base);
                e.col = col;
                if e.kind == 0 {
                    e.interp = mode;
                }
            }
            _ => {
                e.row = Some(next);
                e.col = 0;
                next += 1;
            }
        }
    }
}

/// `Shader::finish` of a linked pixel shader: its inputs where the layout
/// puts them.
pub(super) fn lay_out_inputs(inputs: &mut [SigElement]) {
    LINKED.with_borrow(|l| {
        if let Some(l) = l {
            apply(inputs, &l.layout, 0);
        }
    });
}

/// `Shader::finish` of a linked mesh shader: its vertex and primitive
/// outputs where the layout puts them.
pub(super) fn lay_out_outputs(outputs: &mut [SigElement], prims: &mut [SigElement]) {
    LINKED.with_borrow(|l| {
        if let Some(l) = l {
            apply(outputs, &l.layout, 0);
            apply(prims, &l.layout, l.layout.prim_base);
        }
    });
}

/// The arbitrary inputs a translated pixel shader reads, from its `ISG1`.
pub(super) fn pixel_input_reads(dxil: &[u8]) -> Result<KeptOutputs, ShaderError> {
    use crate::dxbc::u32_at;
    let c = Container::parse(dxil)?;
    let mut reads = KeptOutputs::new();
    let Some(part) = c.part(FourCC::ISG1) else {
        return Ok(reads);
    };
    let data = part.data;
    let count = u32_at(data, 0)? as usize;
    let base = u32_at(data, 4)? as usize;
    for i in 0..count {
        let at = base + 32 * i;
        if u32_at(data, at + 12)? != 0 {
            continue;
        }
        let name_at = u32_at(data, at + 4)? as usize;
        let name = data
            .get(name_at..)
            .and_then(|s| s.split(|&b| b == 0).next())
            .ok_or(ShaderError::Malformed("signature name out of bounds"))?;
        let index = u32_at(data, at + 8)?;
        let (mask, rw) = match data.get(at + 24..at + 26) {
            Some(&[m, r]) => (m, r),
            _ => return Err(ShaderError::Malformed("truncated")),
        };
        let rel = (rw & mask) >> mask.trailing_zeros().min(7);
        if rel != 0 {
            let key = (String::from_utf8_lossy(name).to_ascii_uppercase(), index);
            *reads.entry(key).or_insert(0) |= rel;
        }
    }
    Ok(reads)
}

/// Whether a translated mesh shader's outputs at its declared maxima fit
/// the host's 32 KiB by D3D12's count and by Metal's aligned layout
/// (`lower::mesh_outputs_fit`). None for another stage or a container
/// that does not read.
pub fn mesh_fits_metal(dxil: &[u8]) -> Option<bool> {
    let c = Container::parse(dxil).ok()?;
    let prims = super::signature(c.part(FourCC::PSG1)?.data).ok()?;
    let outputs = match c.part(FourCC::OSG1) {
        Some(part) => super::signature(part.data).ok()?,
        None => Vec::new(),
    };
    let psv = c.part(FourCC::PSV0)?.data;
    let info = psv.get(4..4 + crate::dxbc::u32_at(psv, 0).ok()? as usize)?;
    if *info.get(24)? != 13 {
        return None;
    }
    let half = |at: usize| {
        Some(u32::from(u16::from_le_bytes([
            *info.get(at)?,
            *info.get(at + 1)?,
        ])))
    };
    Some(super::lower::mesh_outputs_fit(
        &outputs,
        &prims,
        half(12)?,
        half(14)?,
        true,
    ))
}

/// The refusal of a linked pair whose mesh outputs still do not fit.
pub(crate) const STILL_OVER: &str = "mesh outputs the pixel shader reads pass Metal's 32 KiB";

/// Translates a mesh shader with only the outputs its pixel shader reads
/// (none without one, a depth-only draw), and the pixel shader with the
/// same inputs, both laid out alike ([`layout`]); refused when the mesh
/// outputs still pass Metal's 32 KiB.
pub fn translate_linked(
    mesh: &[u8],
    pixel: Option<&[u8]>,
) -> Result<(HostShader, Option<HostShader>), ShaderError> {
    link_with(
        mesh,
        || super::translate(mesh),
        pixel.map(|p| move || super::translate(p)),
    )
}

/// [`translate_linked`] of the mesh container `container` with the two
/// translations given, for testing.
pub(super) fn link_with(
    container: &[u8],
    mesh: impl Fn() -> Result<HostShader, ShaderError>,
    pixel: Option<impl Fn() -> Result<HostShader, ShaderError>>,
) -> Result<(HostShader, Option<HostShader>), ShaderError> {
    let (reads, modes) = match &pixel {
        Some(ps) => {
            let (full, modes) = noting_pixel_modes(ps);
            let full = full?;
            if full.stage != HwStage::Ps {
                return Err(ShaderError::Malformed(
                    "the mesh shader's pair is no pixel shader",
                ));
            }
            (pixel_input_reads(&full.dxil)?, modes)
        }
        None => (KeptOutputs::new(), HashMap::new()),
    };
    // The mesh shader's outputs as its lowering keeps them (`emit_program`,
    // `Lower::new`).
    let c = Container::parse(container)?;
    let sig = |fourcc| match c.part(fourcc) {
        Some(part) => super::signature(part.data),
        None => Ok(Vec::new()),
    };
    let mut vertex = sig(FourCC::OSG1)?;
    let mut prims = sig(FourCC::PSG1)?;
    prims.retain(|e| !matches!(e.kind, 10 | 30));
    prune_signature(&mut vertex, &reads);
    prune_signature(&mut prims, &reads);
    let linked = Linked {
        layout: layout(&vertex, &prims, &modes),
        keep: reads,
    };
    let ms = with_linked(linked.clone(), &mesh)?;
    if ms.stage != HwStage::Ms {
        return Err(ShaderError::Malformed(
            "linked as a mesh shader but not one",
        ));
    }
    if mesh_fits_metal(&ms.dxil) != Some(true) {
        return Err(ShaderError::Unsupported {
            offset: 0,
            what: STILL_OVER,
        });
    }
    let ps = match pixel {
        Some(ps) => Some(with_linked(linked, ps)?),
        None => None,
    };
    Ok((ms, ps))
}
