//! The asset database: identity, reference counting, and dependency
//! tracking for registered assets.
//!
//! This is bookkeeping only — no file I/O, no decoding, no GPU upload.
//! Those are later Milestone 5 features (importers); this is the registry
//! they'll register into.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::AssetError;
use crate::id::AssetId;

#[derive(Debug, Default)]
struct Entry {
    ref_count: u32,
    /// Assets this one depends on (e.g. a mesh asset depending on a
    /// texture asset).
    dependencies: HashSet<AssetId>,
}

/// Tracks every currently-registered asset: its reference count and its
/// dependencies on other registered assets.
///
/// An asset is registered with [`AssetDatabase::register`] (ref count 0),
/// gains references via [`AssetDatabase::retain`], and is automatically
/// dropped from the database the moment [`AssetDatabase::release`] brings
/// its count back to zero — same lifecycle shape as `Rc`/`Arc`, just
/// tracked centrally instead of per-handle.
#[derive(Debug, Default)]
pub struct AssetDatabase {
    entries: HashMap<AssetId, Entry>,
}

impl AssetDatabase {
    /// An empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a new asset and returns its freshly generated
    /// [`AssetId`], with a reference count of 0.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_asset::AssetDatabase;
    ///
    /// let mut db = AssetDatabase::new();
    /// let id = db.register();
    /// assert_eq!(db.ref_count(id), Some(0));
    /// ```
    pub fn register(&mut self) -> AssetId {
        let id = AssetId::new();
        self.entries.insert(id, Entry::default());
        id
    }

    /// Whether `id` is currently registered.
    pub fn is_registered(&self, id: AssetId) -> bool {
        self.entries.contains_key(&id)
    }

    /// `id`'s current reference count, or `None` if it isn't registered.
    pub fn ref_count(&self, id: AssetId) -> Option<u32> {
        self.entries.get(&id).map(|entry| entry.ref_count)
    }

    /// Increments `id`'s reference count.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::UnknownAsset`] if `id` isn't registered.
    pub fn retain(&mut self, id: AssetId) -> Result<(), AssetError> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or(AssetError::UnknownAsset(id))?;
        entry.ref_count += 1;
        Ok(())
    }

    /// Decrements `id`'s reference count. If it reaches zero, `id` is
    /// removed from the database entirely (along with its dependency
    /// edges).
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::UnknownAsset`] if `id` isn't registered, or
    /// [`AssetError::NotReferenced`] if its reference count is already 0.
    pub fn release(&mut self, id: AssetId) -> Result<(), AssetError> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or(AssetError::UnknownAsset(id))?;
        if entry.ref_count == 0 {
            return Err(AssetError::NotReferenced(id));
        }
        entry.ref_count -= 1;
        if entry.ref_count == 0 {
            self.entries.remove(&id);
        }
        Ok(())
    }

    /// Records that `dependent` depends on `dependency` (e.g. a mesh
    /// asset depending on the texture asset it references).
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::UnknownAsset`] if either id isn't registered,
    /// or [`AssetError::CyclicDependency`] if `dependency` already
    /// (transitively) depends on `dependent` — the dependency graph must
    /// stay a DAG.
    pub fn add_dependency(
        &mut self,
        dependent: AssetId,
        dependency: AssetId,
    ) -> Result<(), AssetError> {
        if !self.is_registered(dependent) {
            return Err(AssetError::UnknownAsset(dependent));
        }
        if !self.is_registered(dependency) {
            return Err(AssetError::UnknownAsset(dependency));
        }
        if dependent == dependency || self.has_path(dependency, dependent) {
            return Err(AssetError::CyclicDependency {
                dependent,
                dependency,
            });
        }

        if let Some(entry) = self.entries.get_mut(&dependent) {
            entry.dependencies.insert(dependency);
        }
        Ok(())
    }

    /// The set of assets `id` directly depends on, or `None` if `id`
    /// isn't registered.
    pub fn dependencies(&self, id: AssetId) -> Option<&HashSet<AssetId>> {
        self.entries.get(&id).map(|entry| &entry.dependencies)
    }

    /// Breadth-first search: is there a dependency path from `from` to
    /// `to` (following `dependencies` edges)?
    fn has_path(&self, from: AssetId, to: AssetId) -> bool {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([from]);

        while let Some(current) = queue.pop_front() {
            if current == to {
                return true;
            }
            if !visited.insert(current) {
                continue;
            }
            if let Some(entry) = self.entries.get(&current) {
                queue.extend(entry.dependencies.iter().copied());
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_starts_at_zero_ref_count() {
        let mut db = AssetDatabase::new();
        let id = db.register();
        assert_eq!(db.ref_count(id), Some(0));
        assert!(db.is_registered(id));
    }

    #[test]
    fn unregistered_id_reports_none() {
        let db = AssetDatabase::new();
        assert_eq!(db.ref_count(AssetId::new()), None);
        assert!(!db.is_registered(AssetId::new()));
    }

    #[test]
    fn retain_increments_ref_count() {
        let mut db = AssetDatabase::new();
        let id = db.register();
        db.retain(id).unwrap();
        db.retain(id).unwrap();
        assert_eq!(db.ref_count(id), Some(2));
    }

    #[test]
    fn retain_unknown_id_errors() {
        let mut db = AssetDatabase::new();
        let ghost = AssetDatabase::new().register();
        assert_eq!(db.retain(ghost), Err(AssetError::UnknownAsset(ghost)));
    }

    #[test]
    fn release_decrements_and_removes_at_zero() {
        let mut db = AssetDatabase::new();
        let id = db.register();
        db.retain(id).unwrap();
        db.retain(id).unwrap();

        db.release(id).unwrap();
        assert_eq!(db.ref_count(id), Some(1));
        assert!(db.is_registered(id));

        db.release(id).unwrap();
        assert_eq!(db.ref_count(id), None);
        assert!(!db.is_registered(id));
    }

    #[test]
    fn release_below_zero_errors() {
        let mut db = AssetDatabase::new();
        let id = db.register();
        assert_eq!(db.release(id), Err(AssetError::NotReferenced(id)));
    }

    #[test]
    fn release_unknown_id_errors() {
        let mut db = AssetDatabase::new();
        let ghost = AssetDatabase::new().register();
        assert_eq!(db.release(ghost), Err(AssetError::UnknownAsset(ghost)));
    }

    #[test]
    fn add_dependency_records_edge() {
        let mut db = AssetDatabase::new();
        let mesh = db.register();
        let texture = db.register();
        db.add_dependency(mesh, texture).unwrap();

        assert!(db.dependencies(mesh).unwrap().contains(&texture));
        assert!(db.dependencies(texture).unwrap().is_empty());
    }

    #[test]
    fn add_dependency_rejects_unknown_dependent() {
        let mut db = AssetDatabase::new();
        let texture = db.register();
        let ghost = AssetDatabase::new().register();
        assert_eq!(
            db.add_dependency(ghost, texture),
            Err(AssetError::UnknownAsset(ghost))
        );
    }

    #[test]
    fn add_dependency_rejects_unknown_dependency() {
        let mut db = AssetDatabase::new();
        let mesh = db.register();
        let ghost = AssetDatabase::new().register();
        assert_eq!(
            db.add_dependency(mesh, ghost),
            Err(AssetError::UnknownAsset(ghost))
        );
    }

    #[test]
    fn add_dependency_rejects_self_cycle() {
        let mut db = AssetDatabase::new();
        let id = db.register();
        assert_eq!(
            db.add_dependency(id, id),
            Err(AssetError::CyclicDependency {
                dependent: id,
                dependency: id
            })
        );
    }

    #[test]
    fn add_dependency_rejects_direct_cycle() {
        let mut db = AssetDatabase::new();
        let a = db.register();
        let b = db.register();
        db.add_dependency(a, b).unwrap();

        assert_eq!(
            db.add_dependency(b, a),
            Err(AssetError::CyclicDependency {
                dependent: b,
                dependency: a
            })
        );
    }

    #[test]
    fn add_dependency_rejects_transitive_cycle() {
        let mut db = AssetDatabase::new();
        let a = db.register();
        let b = db.register();
        let c = db.register();
        db.add_dependency(a, b).unwrap();
        db.add_dependency(b, c).unwrap();

        // c -> a would close the loop a -> b -> c -> a.
        assert_eq!(
            db.add_dependency(c, a),
            Err(AssetError::CyclicDependency {
                dependent: c,
                dependency: a
            })
        );
    }

    #[test]
    fn diamond_dependency_is_allowed() {
        // a depends on both b and c, which both depend on d. Not a cycle.
        let mut db = AssetDatabase::new();
        let a = db.register();
        let b = db.register();
        let c = db.register();
        let d = db.register();
        db.add_dependency(a, b).unwrap();
        db.add_dependency(a, c).unwrap();
        db.add_dependency(b, d).unwrap();
        db.add_dependency(c, d).unwrap();

        assert!(db.dependencies(a).unwrap().contains(&b));
        assert!(db.dependencies(a).unwrap().contains(&c));
    }
}
