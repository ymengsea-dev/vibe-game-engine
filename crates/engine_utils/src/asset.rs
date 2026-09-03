//! Asset identity and a generic, ref-counted store for live asset data.
//!
//! Lives here rather than in `engine_asset` (the import/registry crate)
//! because both directions need it: `engine_asset` sits *above*
//! `engine_renderer` (importers reference its plain math types), so a
//! renderer-side GPU asset store cannot depend on `engine_asset` without
//! creating a cycle. `engine_utils` sits below both, so [`AssetId`],
//! [`AssetHandle`], and [`AssetStore`] live here instead; `engine_asset`
//! re-exports [`AssetId`] unchanged so existing `engine_asset::AssetId`
//! paths keep working.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use uuid::Uuid;

/// A unique, stable identifier for an asset.
///
/// Backed by a v4 (random) UUID rather than, say, a file path — paths
/// change (renames, reorganized folders); this stays stable across those,
/// which matters once scenes/prefabs start referencing assets by ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetId(Uuid);

impl AssetId {
    /// Generates a new, random asset ID.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_utils::AssetId;
    ///
    /// let a = AssetId::new();
    /// let b = AssetId::new();
    /// assert_ne!(a, b);
    /// ```
    #[allow(
        clippy::new_without_default,
        reason = "Default would imply a meaningful zero value; every AssetId must be freshly random"
    )]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing UUID as an asset ID (e.g. one loaded from a
    /// serialized asset reference).
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<Uuid> for AssetId {
    fn from(uuid: Uuid) -> Self {
        Self::from_uuid(uuid)
    }
}

/// A typed, zero-cost reference to an asset of kind `T`, resolved through
/// an [`AssetStore<T>`] rather than owning `T` directly.
///
/// Wraps an [`AssetId`] plus a phantom marker so `AssetHandle<Mesh>` and
/// `AssetHandle<Texture>` are distinct types at compile time even though
/// both are, underneath, just a UUID — a component can't accidentally pass
/// a texture handle where a mesh handle is expected. `PhantomData<fn() ->
/// T>` (rather than `PhantomData<T>`) keeps every trait impl below
/// unconditional on `T`'s own traits — the handle is an identity, not a
/// value, so it stays `Copy`/`Debug`/... no matter what `T` is.
pub struct AssetHandle<T> {
    id: AssetId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> AssetHandle<T> {
    /// Wraps `id` as a handle to an asset of kind `T`.
    ///
    /// Public so an [`AssetStore<T>`] in another crate (e.g.
    /// `engine_renderer`) can mint handles without this module knowing
    /// about that crate's asset types.
    pub fn new(id: AssetId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// This handle's underlying identity.
    pub fn id(&self) -> AssetId {
        self.id
    }
}

impl<T> Clone for AssetHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for AssetHandle<T> {}

impl<T> PartialEq for AssetHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for AssetHandle<T> {}

impl<T> Hash for AssetHandle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> fmt::Debug for AssetHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AssetHandle").field(&self.id).finish()
    }
}

/// One stored asset plus how many live handles currently share it.
struct Entry<T> {
    value: T,
    ref_count: u32,
}

/// A generic, ref-counted store of live asset data, keyed by [`AssetId`].
///
/// [`insert`](Self::insert) registers a value with a ref count of `1` and
/// returns the handle its first owner holds. A second owner that wants to
/// share the same underlying asset calls [`retain`](Self::retain) on that
/// handle (bumping the count) rather than inserting a duplicate copy.
/// [`release`](Self::release) is the inverse — call it once per owner that
/// stops referencing a handle (e.g. on entity despawn); the entry is
/// dropped the moment the count reaches zero, the same lifecycle shape as
/// `engine_asset::AssetDatabase`, just holding the data itself rather than
/// bookkeeping a separate registry.
///
/// Not thread-safe (plain `HashMap`, no interior mutability) — matches
/// every other GPU-resource owner in this engine (`GpuContext`,
/// `PhysicsWorld`, ...), which are all single-threaded, caller-owned state.
pub struct AssetStore<T> {
    entries: HashMap<AssetId, Entry<T>>,
}

impl<T> AssetStore<T> {
    /// An empty store.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Registers `value` as a new asset with a ref count of `1` and
    /// returns a handle to it.
    pub fn insert(&mut self, value: T) -> AssetHandle<T> {
        let id = AssetId::new();
        self.entries.insert(
            id,
            Entry {
                value,
                ref_count: 1,
            },
        );
        AssetHandle::new(id)
    }

    /// Increments `handle`'s ref count — call this when a second owner
    /// starts sharing an asset another owner already holds a handle to.
    ///
    /// Returns `false` (does nothing) if `handle` isn't registered — a
    /// stale or foreign handle is a valid, non-corrupting state to observe
    /// (see [`get`](Self::get)'s docs), not a panic.
    pub fn retain(&mut self, handle: AssetHandle<T>) -> bool {
        match self.entries.get_mut(&handle.id()) {
            Some(entry) => {
                entry.ref_count += 1;
                true
            }
            None => false,
        }
    }

    /// Decrements `handle`'s ref count, removing the entry entirely once
    /// it reaches zero.
    ///
    /// Returns `true` if this call actually removed the entry, `false` if
    /// it either survived (still referenced elsewhere) or `handle` wasn't
    /// registered to begin with.
    ///
    /// # Panics
    ///
    /// Never — but calling this more times than the matching
    /// `insert`+`retain` calls for `handle` is a caller bug (each `insert`
    /// or `retain` must have exactly one matching `release`); once the
    /// count reaches zero the entry is gone, so an extra `release` just
    /// returns `false` rather than underflowing.
    pub fn release(&mut self, handle: AssetHandle<T>) -> bool {
        let Some(entry) = self.entries.get_mut(&handle.id()) else {
            return false;
        };
        entry.ref_count -= 1;
        if entry.ref_count == 0 {
            self.entries.remove(&handle.id());
            true
        } else {
            false
        }
    }

    /// The asset `handle` refers to, or `None` if it isn't (or is no
    /// longer) registered.
    ///
    /// A missing handle is a valid, expected state — e.g. one frame's
    /// worth of stale references after a [`release`](Self::release) — not
    /// an error; callers should skip whatever they were about to draw with
    /// it, not treat it as corruption.
    pub fn get(&self, handle: AssetHandle<T>) -> Option<&T> {
        self.entries.get(&handle.id()).map(|entry| &entry.value)
    }

    /// A mutable reference to the asset `handle` refers to, or `None` if it
    /// isn't (or is no longer) registered.
    ///
    /// For editing a stored asset in place — e.g. re-uploading a sculpted
    /// terrain mesh's vertices — without changing its identity or ref
    /// count. A missing handle is the same valid, expected state
    /// [`get`](Self::get) documents, not an error.
    pub fn get_mut(&mut self, handle: AssetHandle<T>) -> Option<&mut T> {
        self.entries
            .get_mut(&handle.id())
            .map(|entry| &mut entry.value)
    }

    /// `handle`'s current ref count, or `None` if it isn't registered.
    pub fn ref_count(&self, handle: AssetHandle<T>) -> Option<u32> {
        self.entries.get(&handle.id()).map(|entry| entry.ref_count)
    }

    /// Number of distinct assets currently registered (not the sum of
    /// their ref counts).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no assets are currently registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<T> Default for AssetStore<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique() {
        assert_ne!(AssetId::new(), AssetId::new());
    }

    #[test]
    fn from_uuid_round_trips() {
        let uuid = Uuid::new_v4();
        assert_eq!(AssetId::from_uuid(uuid).as_uuid(), uuid);
    }

    #[test]
    fn display_matches_uuid_display() {
        let uuid = Uuid::new_v4();
        let id = AssetId::from_uuid(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }

    #[test]
    fn handle_equality_and_hash_follow_id() {
        let id = AssetId::new();
        let a = AssetHandle::<u32>::new(id);
        let b = AssetHandle::<u32>::new(id);
        assert_eq!(a, b);
        assert_eq!(a.id(), id);
    }

    #[test]
    fn handle_debug_does_not_require_t_debug() {
        struct NotDebug;
        let handle = AssetHandle::<NotDebug>::new(AssetId::new());
        // Compiles and doesn't panic — `AssetHandle<T>: Debug` regardless
        // of whether `T: Debug`.
        let _ = format!("{handle:?}");
    }

    #[test]
    fn insert_starts_at_ref_count_one() {
        let mut store = AssetStore::new();
        let handle = store.insert(42);
        assert_eq!(store.ref_count(handle), Some(1));
        assert_eq!(store.get(handle), Some(&42));
    }

    #[test]
    fn retain_increments_ref_count() {
        let mut store = AssetStore::new();
        let handle = store.insert("mesh");
        assert!(store.retain(handle));
        assert_eq!(store.ref_count(handle), Some(2));
    }

    #[test]
    fn retain_on_unknown_handle_returns_false() {
        let mut store: AssetStore<u32> = AssetStore::new();
        let ghost = AssetHandle::new(AssetId::new());
        assert!(!store.retain(ghost));
    }

    #[test]
    fn release_removes_entry_once_ref_count_hits_zero() {
        let mut store = AssetStore::new();
        let handle = store.insert("texture");
        store.retain(handle);

        assert!(!store.release(handle));
        assert!(store.get(handle).is_some());

        assert!(store.release(handle));
        assert!(store.get(handle).is_none());
        assert_eq!(store.ref_count(handle), None);
    }

    #[test]
    fn release_on_unknown_handle_returns_false() {
        let mut store: AssetStore<u32> = AssetStore::new();
        let ghost = AssetHandle::new(AssetId::new());
        assert!(!store.release(ghost));
    }

    #[test]
    fn extra_release_after_removal_is_a_harmless_no_op() {
        let mut store = AssetStore::new();
        let handle = store.insert(1);
        assert!(store.release(handle));
        // The entry is already gone; a second release just reports it
        // found nothing, rather than underflowing.
        assert!(!store.release(handle));
    }

    #[test]
    fn get_on_unregistered_handle_is_none_not_a_panic() {
        let store: AssetStore<u32> = AssetStore::new();
        let ghost = AssetHandle::new(AssetId::new());
        assert_eq!(store.get(ghost), None);
    }

    #[test]
    fn get_mut_edits_in_place_and_keeps_ref_count() {
        let mut store = AssetStore::new();
        let handle = store.insert(10u32);
        *store.get_mut(handle).expect("just inserted") += 5;
        assert_eq!(store.get(handle), Some(&15));
        assert_eq!(store.ref_count(handle), Some(1));
    }

    #[test]
    fn get_mut_on_released_handle_is_none() {
        let mut store = AssetStore::new();
        let handle = store.insert(1u32);
        assert!(store.release(handle));
        assert_eq!(store.get_mut(handle), None);
    }

    #[test]
    fn len_and_is_empty_track_distinct_assets_not_ref_counts() {
        let mut store = AssetStore::new();
        assert!(store.is_empty());
        let handle = store.insert("a");
        store.retain(handle);
        store.retain(handle);
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn default_is_empty() {
        let store: AssetStore<u32> = AssetStore::default();
        assert!(store.is_empty());
    }
}
