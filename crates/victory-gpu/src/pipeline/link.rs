//! Mesh pipelines over Metal's 32 KiB of outputs, on D3DMetal: the mesh
//! shader linked with its pixel shader when the pipeline is made
//! (`victory_shader::emit::translate_linked`), or the pipeline refused.
//!
//! D3DMetal makes a mesh pipeline whose outputs pass 32 KiB by Metal's
//! aligned layout, and its submission then never completes (docs/macos.md).
//! With linking on (`VICTORY_LINK_MESH_OUTPUTS`, on unless `0`; only
//! D3DMetal links) such a mesh shader is translated alone without being
//! refused, and [`plan`] decides here: the pair translated again with only
//! the outputs the pixel shader reads, if that fits; else the pipeline is
//! refused (its draws are dropped, the queue keeps running), or with
//! `VICTORY_MESH_OVER_LIMIT=1` made alone anyway. Pairs are kept by both
//! containers' full hashes, failures too. Elsewhere, and with
//! `VICTORY_LINK_MESH_OUTPUTS=0`, nothing changes: the translator refuses
//! such a shader on D3DMetal as before.
//!
//! Ported from pragyaangaur/VIctory-macOS PR #161/#401/#282.

use std::collections::HashMap;
use std::sync::Mutex;

/// `VICTORY_LINK_MESH_OUTPUTS`: on unless `0`; takes effect on D3DMetal
/// alone (`crate::host`).
#[cfg(windows)]
pub(crate) fn requested() -> bool {
    std::env::var_os("VICTORY_LINK_MESH_OUTPUTS").is_none_or(|v| v != "0")
}

/// A pair's key: the mesh container's full hash, the pixel container's
/// (none for a depth-only pipeline).
pub(crate) type Key = ([u8; 32], Option<[u8; 32]>);

/// Linked pairs by [`Key`]; `None`: the pair did not link (not tried again).
pub(crate) struct Pairs<T> {
    pairs: Mutex<Option<HashMap<Key, Option<T>>>>,
}

impl<T: Clone> Pairs<T> {
    pub(crate) const fn new() -> Self {
        Pairs {
            pairs: Mutex::new(None),
        }
    }

    /// The pair under `key`, linked by `link` the first time.
    pub(crate) fn get_or_link(&self, key: Key, link: impl FnOnce() -> Option<T>) -> Option<T> {
        if let Some(hit) = self
            .pairs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(&key))
        {
            return hit.clone();
        }
        // (linked without the lock: a translation takes a while)
        let made = link();
        self.pairs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .entry(key)
            .or_insert(made)
            .clone()
    }
}

/// What a mesh pipeline is made with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan<T> {
    /// The shaders as translated alone.
    Alone,
    /// The linked pair.
    Linked(T),
    /// No pipeline: its outputs pass the limit even linked.
    Refuse,
}

/// The plan for a mesh pipeline: `linking` on (D3DMetal), whether the mesh
/// shader alone fits (`None`: not known, taken as fitting), the pair
/// (`link`, tried only when it does not), `over_limit`
/// (`VICTORY_MESH_OVER_LIMIT=1`). Pure, for testing.
pub(crate) fn decide<T>(
    linking: bool,
    fits_alone: Option<bool>,
    link: impl FnOnce() -> Option<T>,
    over_limit: bool,
) -> Plan<T> {
    if !linking || fits_alone != Some(false) {
        return Plan::Alone;
    }
    match link() {
        Some(pair) => Plan::Linked(pair),
        None if over_limit => Plan::Alone,
        None => Plan::Refuse,
    }
}

#[cfg(windows)]
mod host {
    use std::sync::Arc;

    use super::super::Shader;
    use super::{Pairs, Plan};

    /// A linked mesh shader and its pixel shader (none: depth only).
    pub(crate) type Pair = (Arc<Shader>, Option<Arc<Shader>>);

    static PAIRS: Pairs<Pair> = Pairs::new();

    /// The plan for the mesh pipeline of `ms` (translated) and `ps`.
    pub(crate) fn plan(ms: &Shader, ps: Option<&Shader>) -> Plan<Pair> {
        use victory_shader::emit::{host_mesh_link, mesh_fits_metal, mesh_over_limit_allowed};
        let fits = ms.translated().and_then(|t| mesh_fits_metal(&t.host.dxil));
        let link = || PAIRS.get_or_link((ms.full, ps.map(|p| p.full)), || linked(ms, ps));
        let plan = super::decide(host_mesh_link(), fits, link, mesh_over_limit_allowed());
        if matches!(plan, Plan::Alone) && fits == Some(false) && host_mesh_link() {
            tracing::warn!(mesh = %ms.hash, "mesh outputs over Metal's 32 KiB, not linked: made anyway (VICTORY_MESH_OVER_LIMIT=1)");
        }
        plan
    }

    fn linked(ms: &Shader, ps: Option<&Shader>) -> Option<Pair> {
        let pixel = ps.map(|p| p.container.as_slice());
        let pixel_hash = ps.map_or("none", |p| p.hash.as_str());
        let made = victory_shader::emit::translate_linked(&ms.container, pixel)
            .map_err(|e| e.to_string())
            .and_then(|(m, p)| {
                let mesh = ms.with_translation(m)?;
                let pixel = match (ps, p) {
                    (Some(s), Some(t)) => Some(s.with_translation(t)?),
                    _ => None,
                };
                Ok((mesh, pixel))
            });
        match made {
            Ok(pair) => {
                tracing::info!(mesh = %ms.hash, pixel = pixel_hash, "mesh shader linked with its pixel shader under Metal's 32 KiB");
                Some(pair)
            }
            Err(error) => {
                tracing::warn!(mesh = %ms.hash, pixel = pixel_hash, %error, "mesh and pixel shaders do not link");
                None
            }
        }
    }
}

#[cfg(windows)]
pub(crate) use host::plan;

#[cfg(test)]
mod tests {
    use super::*;

    /// A pair is kept by both full hashes: the same mesh shader with
    /// another pixel shader (or none) links again; a failure is kept too.
    #[test]
    fn pairs_are_kept_by_both_hashes() {
        let pairs: Pairs<u32> = Pairs::new();
        let (ms, ps, other) = ([1; 32], [2; 32], [3; 32]);
        let mut runs = 0;
        let mut link = |v: Option<u32>| {
            runs += 1;
            v
        };
        assert_eq!(pairs.get_or_link((ms, Some(ps)), || link(Some(1))), Some(1));
        assert_eq!(pairs.get_or_link((ms, Some(ps)), || link(Some(9))), Some(1));
        assert_eq!(
            pairs.get_or_link((ms, Some(other)), || link(Some(2))),
            Some(2)
        );
        assert_eq!(pairs.get_or_link((ms, None), || link(None)), None);
        assert_eq!(
            pairs.get_or_link((ms, None), || link(Some(9))),
            None,
            "a failure is kept"
        );
        assert_eq!(pairs.get_or_link((ps, Some(ms)), || link(Some(4))), Some(4));
        assert_eq!(runs, 4);
    }

    /// Linking only when on and the mesh shader alone does not fit; a pair
    /// that does not link falls back on the shader alone with
    /// `VICTORY_MESH_OVER_LIMIT=1`, else the pipeline is refused.
    #[test]
    fn mesh_pipelines_link_or_fall_back() {
        let never = || -> Option<u8> { panic!("not linked") };
        assert_eq!(
            decide(false, Some(false), never, false),
            Plan::Alone,
            "Windows, or linking off"
        );
        assert_eq!(decide(true, Some(true), never, false), Plan::Alone);
        assert_eq!(decide(true, None, never, false), Plan::Alone);
        assert_eq!(
            decide(true, Some(false), || Some(7), false),
            Plan::Linked(7)
        );
        assert_eq!(
            decide(true, Some(false), || None::<u8>, false),
            Plan::Refuse
        );
        assert_eq!(decide(true, Some(false), || None::<u8>, true), Plan::Alone);
    }
}
