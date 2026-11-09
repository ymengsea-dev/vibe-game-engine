//! Linked prefab copies: what a copy changed, and bringing copies up to date.
//!
//! A copy keeps full object data in its scene, so it still works if its prefab
//! file is gone. Each member carries a [`PrefabLink`] naming its object in the
//! prefab and the fields it overrides; every other field follows the prefab.
//! References between members are stored as the copy's own IDs in a scene and
//! as prefab IDs in a prefab, so they are translated before comparing or copying.

use std::collections::{HashMap, HashSet};

use crate::format::{PrefabLink, RuleAction, RuleTrigger, Scene, SceneEntity, SpawnAt};
use crate::prefab::Prefab;

/// Marks, on the top object's overrides, a prefab member this copy deleted.
const REMOVED_PREFIX: &str = "removed:";

/// The name [`changed_fields`] uses for a transform.
const TRANSFORM: &str = "transform";

/// One field a copy can override.
struct Field {
    name: &'static str,
    same: fn(&SceneEntity, &SceneEntity) -> bool,
    copy: fn(&mut SceneEntity, &SceneEntity),
}

macro_rules! field {
    ($name:ident) => {
        Field {
            name: stringify!($name),
            same: |a, b| a.$name == b.$name,
            copy: |to, from| to.$name = from.$name.clone(),
        }
    };
}

/// Everything that follows the prefab. IDs, parent and order describe where a
/// copy sits in its scene, and a lock is an editing aid, so those stay per copy.
const FIELDS: &[Field] = &[
    field!(name),
    field!(script),
    field!(transform),
    field!(camera),
    field!(camera_follow),
    field!(mesh_renderer),
    field!(foliage),
    field!(asset_source),
    field!(asset_id),
    field!(sprite),
    field!(tilemap),
    field!(disabled),
    field!(collider),
    field!(audio_emitter),
    field!(water),
    field!(point_light),
    field!(ui),
    field!(rules),
    field!(tags),
    field!(keep_between_scenes),
];

/// Points everything in `entity` that names an object by ID at `map`'s
/// replacement, where it has one.
pub fn remap_object_references(entity: &mut SceneEntity, map: &HashMap<String, String>) {
    let remap = |id: &mut String| {
        if let Some(new_id) = map.get(id) {
            id.clone_from(new_id);
        }
    };
    if let Some(follow) = &mut entity.camera_follow {
        remap(&mut follow.target);
    }
    for rule in &mut entity.rules {
        match &mut rule.action {
            RuleAction::SetUiText { target, .. }
            | RuleAction::PlayAudio { target }
            | RuleAction::SetVisible { target, .. }
            | RuleAction::SetEnabled { target, .. }
            | RuleAction::Destroy { target } => remap(target),
            RuleAction::SpawnPrefab {
                at: SpawnAt::Object(id),
                ..
            } => remap(id),
            _ => {}
        }
        if let RuleTrigger::CollisionEnter { with: Some(id) }
        | RuleTrigger::CollisionExit { with: Some(id) }
        | RuleTrigger::TriggerEnter { with: Some(id) }
        | RuleTrigger::TriggerExit { with: Some(id) } = &mut rule.trigger
        {
            remap(id);
        }
    }
}

/// How the object at `index` of a prefab is named by links to it. Prefabs from
/// before links have no IDs and only one object.
pub(crate) fn member_key(index: usize, entity: &SceneEntity) -> String {
    entity
        .object_id
        .clone()
        .unwrap_or_else(|| format!("#{index}"))
}

/// The prefab files a scene links to, each read once.
pub struct PrefabFiles<'a> {
    load: &'a mut dyn FnMut(&str) -> Option<Prefab>,
    loaded: HashMap<String, Option<Prefab>>,
}

impl<'a> PrefabFiles<'a> {
    /// Files read through `load`, which returns `None` for a missing or broken one.
    pub fn new(load: &'a mut dyn FnMut(&str) -> Option<Prefab>) -> Self {
        Self {
            load,
            loaded: HashMap::new(),
        }
    }

    fn get(&mut self, path: &str) -> Option<&Prefab> {
        let load = &mut self.load;
        self.loaded
            .entry(path.to_owned())
            .or_insert_with(|| load(path))
            .as_ref()
    }
}

/// One copy in a scene: its top object and linked members, top object first.
struct Copy {
    path: String,
    members: Vec<usize>,
}

fn children_lists(scene: &Scene) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); scene.entities.len()];
    for (index, entity) in scene.entities.iter().enumerate() {
        if let Some(list) = entity.parent.and_then(|parent| children.get_mut(parent)) {
            list.push(index);
        }
    }
    children
}

/// Every object under `root`, not including it. Bounded against cyclic parents.
fn descendants(children: &[Vec<usize>], root: usize) -> Vec<usize> {
    let mut seen = HashSet::from([root]);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(at) = stack.pop() {
        for &child in children.get(at).into_iter().flatten() {
            if seen.insert(child) {
                out.push(child);
                stack.push(child);
            }
        }
    }
    out
}

/// Whether the linked object is the top object of a copy of its prefab.
fn is_copy_root(entity: &SceneEntity, files: &mut PrefabFiles<'_>) -> bool {
    let Some(link) = &entity.prefab else {
        return false;
    };
    files
        .get(&link.path)
        .is_some_and(|prefab| link.member == member_key(0, prefab.entity()))
}

/// The copies in `scene` whose prefab file can be read.
fn find_copies(scene: &Scene, files: &mut PrefabFiles<'_>) -> Vec<Copy> {
    let children = children_lists(scene);
    let mut copies = Vec::new();
    for (root, entity) in scene.entities.iter().enumerate() {
        if !is_copy_root(entity, files) {
            continue;
        }
        let Some(link) = &entity.prefab else { continue };
        let mut members = vec![root];
        let mut stack = vec![root];
        let mut seen = HashSet::from([root]);
        while let Some(at) = stack.pop() {
            for &child in children.get(at).into_iter().flatten() {
                let Some(data) = scene.entities.get(child) else {
                    continue;
                };
                // Another copy nested here looks after itself.
                if !seen.insert(child) || is_copy_root(data, files) {
                    continue;
                }
                if data.prefab.as_ref().is_some_and(|l| l.path == link.path) {
                    members.push(child);
                }
                stack.push(child);
            }
        }
        copies.push(Copy {
            path: link.path.clone(),
            members,
        });
    }
    copies
}

/// The fields `copy` changes from `member`, its object in the prefab, with
/// `copy`'s references already in prefab IDs. A top object's transform says
/// where the copy is, so it never counts.
pub fn changed_fields(copy: &SceneEntity, member: &SceneEntity, is_root: bool) -> Vec<String> {
    FIELDS
        .iter()
        .filter(|field| !(is_root && field.name == TRANSFORM))
        .filter(|field| !(field.same)(copy, member))
        .map(|field| field.name.to_owned())
        .collect()
}

/// Copy IDs to prefab member keys, for one copy.
fn ids_to_members(scene: &Scene, copy: &Copy) -> HashMap<String, String> {
    copy.members
        .iter()
        .filter_map(|&index| {
            let entity = scene.entities.get(index)?;
            Some((
                entity.object_id.clone()?,
                entity.prefab.as_ref()?.member.clone(),
            ))
        })
        .collect()
}

/// Writes each linked object's override list from what it differs in now.
/// Copies of a missing prefab keep the lists they have.
pub fn stamp_overrides(scene: &mut Scene, load: &mut dyn FnMut(&str) -> Option<Prefab>) {
    let mut files = PrefabFiles::new(load);
    for copy in find_copies(scene, &mut files) {
        let Some(prefab) = files.get(&copy.path) else {
            continue;
        };
        let to_member = ids_to_members(scene, &copy);
        let by_key: HashMap<String, &SceneEntity> = prefab
            .entities()
            .enumerate()
            .map(|(index, entity)| (member_key(index, entity), entity))
            .collect();
        let mut present = HashSet::new();
        for (position, &index) in copy.members.iter().enumerate() {
            let Some(entity) = scene.entities.get(index) else {
                continue;
            };
            let Some(key) = entity.prefab.as_ref().map(|link| link.member.clone()) else {
                continue;
            };
            let Some(member) = by_key.get(&key) else {
                continue;
            };
            let mut normal = entity.clone();
            remap_object_references(&mut normal, &to_member);
            let changed = changed_fields(&normal, member, position == 0);
            present.insert(key);
            if let Some(link) = scene
                .entities
                .get_mut(index)
                .and_then(|entity| entity.prefab.as_mut())
            {
                link.overrides = changed;
            }
        }
        let removed: Vec<String> = prefab
            .entities()
            .enumerate()
            .map(|(index, entity)| member_key(index, entity))
            .filter(|key| !present.contains(key))
            .map(|key| format!("{REMOVED_PREFIX}{key}"))
            .collect();
        if let Some(link) = copy
            .members
            .first()
            .and_then(|&root| scene.entities.get_mut(root))
            .and_then(|entity| entity.prefab.as_mut())
        {
            link.overrides.extend(removed);
        }
    }
}

/// Brings every copy up to date with its prefab file: fields it does not
/// override come from the prefab, new prefab objects are added and removed ones
/// deleted. Objects added only to a copy stay. Returns whether anything changed.
pub fn refresh_copies(scene: &mut Scene, load: &mut dyn FnMut(&str) -> Option<Prefab>) -> bool {
    let before = scene.entities.clone();
    let mut files = PrefabFiles::new(load);
    let mut additions: Vec<(SceneEntity, String)> = Vec::new();
    let mut deletions = HashSet::new();
    for copy in find_copies(scene, &mut files) {
        let Some(prefab) = files.get(&copy.path) else {
            continue;
        };
        let keys: HashSet<String> = prefab
            .entities()
            .enumerate()
            .map(|(index, entity)| member_key(index, entity))
            .collect();
        let existing: HashMap<String, usize> = copy
            .members
            .iter()
            .filter_map(|&index| {
                let link = scene.entities.get(index)?.prefab.as_ref()?;
                Some((link.member.clone(), index))
            })
            .collect();
        let root_overrides: HashSet<String> = copy
            .members
            .first()
            .and_then(|&root| scene.entities.get(root)?.prefab.as_ref())
            .map(|link| link.overrides.iter().cloned().collect())
            .unwrap_or_default();
        let is_removed = |key: &str| root_overrides.contains(&format!("{REMOVED_PREFIX}{key}"));

        // Members to the copy's IDs, new objects included.
        let mut to_copy: HashMap<String, String> = HashMap::new();
        for (key, &index) in &existing {
            if let Some(id) = scene.entities.get(index).and_then(|e| e.object_id.clone()) {
                to_copy.insert(key.clone(), id);
            }
        }
        for key in &keys {
            if !existing.contains_key(key) && !is_removed(key) {
                to_copy.insert(key.clone(), uuid::Uuid::new_v4().to_string());
            }
        }

        let root_id = copy
            .members
            .first()
            .and_then(|&root| scene.entities.get(root)?.object_id.clone())
            .unwrap_or_default();
        let list: Vec<&SceneEntity> = prefab.entities().collect();
        for (index, member) in list.iter().enumerate() {
            let key = member_key(index, member);
            let mut data = (*member).clone();
            remap_object_references(&mut data, &to_copy);
            match existing.get(&key) {
                Some(&at) => {
                    let Some(entity) = scene.entities.get_mut(at) else {
                        continue;
                    };
                    let overrides = entity
                        .prefab
                        .as_ref()
                        .map_or_else(Vec::new, |link| link.overrides.clone());
                    for field in FIELDS {
                        let per_copy = (index == 0 && field.name == TRANSFORM)
                            || overrides.iter().any(|name| name == field.name);
                        if !per_copy {
                            (field.copy)(entity, &data);
                        }
                    }
                }
                None if !is_removed(&key) => {
                    data.object_id = to_copy.get(&key).cloned();
                    data.prefab = Some(PrefabLink {
                        path: copy.path.clone(),
                        member: key,
                        overrides: Vec::new(),
                    });
                    let parent_id = data
                        .parent
                        .and_then(|parent| list.get(parent).map(|p| member_key(parent, p)))
                        .and_then(|parent| to_copy.get(&parent).cloned())
                        .unwrap_or_else(|| root_id.clone());
                    additions.push((data, parent_id));
                }
                None => {}
            }
        }
        for (key, &index) in &existing {
            if !keys.contains(key)
                && let Some(id) = scene.entities.get(index).and_then(|e| e.object_id.clone())
            {
                deletions.insert(id);
            }
        }
    }
    let first_new = scene.entities.len();
    let parents: Vec<String> = additions.iter().map(|(_, parent)| parent.clone()).collect();
    scene
        .entities
        .extend(additions.into_iter().map(|(entity, _)| entity));
    let index_of: HashMap<String, usize> = scene
        .entities
        .iter()
        .enumerate()
        .filter_map(|(index, entity)| Some((entity.object_id.clone()?, index)))
        .collect();
    for (offset, parent) in parents.iter().enumerate() {
        if let Some(entity) = scene.entities.get_mut(first_new + offset) {
            entity.parent = index_of.get(parent).copied();
        }
    }
    scene.remove_objects(&deletions);
    scene.entities != before
}

/// Where the top object of the copy holding `object_id` is, if it is in one.
pub fn copy_root_of(
    scene: &Scene,
    object_id: &str,
    load: &mut dyn FnMut(&str) -> Option<Prefab>,
) -> Option<usize> {
    let mut files = PrefabFiles::new(load);
    let mut at = scene
        .entities
        .iter()
        .position(|entity| entity.object_id.as_deref() == Some(object_id))?;
    for _ in 0..=scene.entities.len() {
        let entity = scene.entities.get(at)?;
        if is_copy_root(entity, &mut files) {
            return Some(at);
        }
        at = entity.parent?;
    }
    None
}

/// The names of what the copy at `root` changes: `field`, or `Child: field` for
/// an object under it. Assumes `stamp_overrides` has run.
pub fn copy_changes(scene: &Scene, root: usize) -> Vec<String> {
    let children = children_lists(scene);
    let top_path = scene
        .entities
        .get(root)
        .and_then(|entity| entity.prefab.as_ref())
        .map(|link| link.path.as_str());
    let mut out = Vec::new();
    for index in std::iter::once(root).chain(descendants(&children, root)) {
        let Some(entity) = scene.entities.get(index) else {
            continue;
        };
        let label = entity.name.as_deref().unwrap_or("Object");
        match &entity.prefab {
            Some(link) if Some(link.path.as_str()) == top_path => {
                for name in &link.overrides {
                    if index == root && !name.starts_with(REMOVED_PREFIX) {
                        out.push(name.clone());
                    } else if name.starts_with(REMOVED_PREFIX) {
                        out.push("an object was deleted".to_owned());
                    } else {
                        out.push(format!("{label}: {name}"));
                    }
                }
            }
            _ => out.push(format!("{label}: added")),
        }
    }
    out
}

/// Makes the copy at `root` the new prefab: everything under it, except that
/// the top object's transform stays as it is in `old`, because that says where
/// the copy is. Objects added to the copy join the prefab and get linked, and
/// the copy's overrides are cleared.
pub fn apply_copy(scene: &mut Scene, root: usize, old: &Prefab) -> Option<Prefab> {
    let path = scene.entities.get(root)?.prefab.as_ref()?.path.clone();
    let children = children_lists(scene);
    let order: Vec<usize> = std::iter::once(root)
        .chain(descendants(&children, root))
        .collect();
    let mut keys: HashMap<String, String> = HashMap::new();
    for &index in &order {
        let entity = scene.entities.get(index)?;
        let id = entity.object_id.clone()?;
        let key = entity
            .prefab
            .as_ref()
            .filter(|link| link.path == path)
            .map_or_else(|| id.clone(), |link| link.member.clone());
        keys.insert(id, key);
    }
    let position: HashMap<usize, usize> = order
        .iter()
        .enumerate()
        .map(|(position, &index)| (index, position))
        .collect();
    let mut entities = Vec::with_capacity(order.len());
    for (at, &index) in order.iter().enumerate() {
        let source = scene.entities.get(index)?;
        let key = keys.get(source.object_id.as_ref()?)?.clone();
        let mut data = source.clone();
        remap_object_references(&mut data, &keys);
        // A prefab from before links has no ID on its one object.
        data.object_id = (!key.starts_with('#')).then(|| key.clone());
        data.prefab = None;
        data.locked = false;
        data.parent = source
            .parent
            .filter(|_| at > 0)
            .and_then(|parent| position.get(&parent).copied());
        if at == 0 {
            data.transform = old.entity().transform;
        }
        entities.push(data);
        if let Some(entity) = scene.entities.get_mut(index) {
            entity.prefab = Some(PrefabLink {
                path: path.clone(),
                member: key,
                overrides: Vec::new(),
            });
        }
    }
    Prefab::from_entities(entities)
}

/// Clears the copy at `root` back to following its prefab: overrides go, and
/// objects under it that are not part of the prefab are removed. Run
/// [`refresh_copies`] next to bring back the prefab's values.
pub fn revert_copy(scene: &mut Scene, root: usize) {
    let Some(path) = scene
        .entities
        .get(root)
        .and_then(|entity| entity.prefab.as_ref())
        .map(|link| link.path.clone())
    else {
        return;
    };
    let children = children_lists(scene);
    let mut extras = HashSet::new();
    for index in std::iter::once(root).chain(descendants(&children, root)) {
        let Some(entity) = scene.entities.get_mut(index) else {
            continue;
        };
        match &mut entity.prefab {
            Some(link) if link.path == path => link.overrides.clear(),
            _ => {
                if let Some(id) = &entity.object_id {
                    extras.insert(id.clone());
                }
            }
        }
    }
    scene.remove_objects(&extras);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{RuleData, TransformData};

    const PATH: &str = "assets/prefabs/a.prefab";
    const TOP: &str = "00000000-0000-4000-8000-000000000001";
    const KID: &str = "00000000-0000-4000-8000-000000000002";

    fn at(x: f32) -> Option<TransformData> {
        Some(TransformData {
            translation: [x, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        })
    }

    fn named(name: &str, id: &str, x: f32, parent: Option<usize>) -> SceneEntity {
        SceneEntity {
            name: Some(name.into()),
            object_id: Some(id.into()),
            transform: at(x),
            parent,
            ..Default::default()
        }
    }

    fn prefab() -> Prefab {
        Prefab::from_entities(vec![
            named("Top", TOP, 0.0, None),
            named("Kid", KID, 1.0, Some(0)),
        ])
        .unwrap()
    }

    fn placed(prefab: &Prefab) -> Scene {
        let mut entities = prefab.copy_entities(Some(PATH));
        entities[0].transform = at(50.0);
        Scene {
            entities,
            ..Default::default()
        }
    }

    fn load(prefab: &Prefab) -> impl FnMut(&str) -> Option<Prefab> + use<> {
        let prefab = prefab.clone();
        move |path| (path == PATH).then(|| prefab.clone())
    }

    fn find<'a>(scene: &'a Scene, name: &str) -> &'a SceneEntity {
        scene
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some(name))
            .unwrap()
    }

    fn find_mut<'a>(scene: &'a mut Scene, name: &str) -> &'a mut SceneEntity {
        scene
            .entities
            .iter_mut()
            .find(|entity| entity.name.as_deref() == Some(name))
            .unwrap()
    }

    #[test]
    fn a_new_copy_has_no_changes_and_its_position_does_not_count() {
        let base = prefab();
        let mut scene = placed(&base);
        stamp_overrides(&mut scene, &mut load(&base));
        for entity in &scene.entities {
            assert!(entity.prefab.as_ref().unwrap().overrides.is_empty());
        }
    }

    #[test]
    fn a_prefab_change_reaches_copies_and_an_override_survives() {
        let mut base = prefab();
        let mut scene = placed(&base);
        find_mut(&mut scene, "Kid").disabled = true;
        stamp_overrides(&mut scene, &mut load(&base));
        assert_eq!(
            find(&scene, "Kid").prefab.as_ref().unwrap().overrides,
            ["disabled"]
        );

        // The prefab changes the kid's script and turns disabled off.
        let mut entities: Vec<_> = base.entities().cloned().collect();
        entities[1].script = Some("assets/scripts/k.lua".into());
        entities[0].tags = vec!["boss".into()];
        base = Prefab::from_entities(entities).unwrap();

        assert!(refresh_copies(&mut scene, &mut load(&base)));
        let kid = find(&scene, "Kid");
        assert_eq!(kid.script.as_deref(), Some("assets/scripts/k.lua"));
        assert!(kid.disabled, "the override stays");
        let top = find(&scene, "Top");
        assert_eq!(top.tags, ["boss"]);
        assert_eq!(top.transform, at(50.0), "the copy stays where it is");
        assert!(!refresh_copies(&mut scene, &mut load(&base)));
    }

    #[test]
    fn prefab_members_added_and_removed_reach_copies() {
        let base = prefab();
        let mut scene = placed(&base);
        let new_kid = "00000000-0000-4000-8000-000000000003";
        let changed = Prefab::from_entities(vec![
            named("Top", TOP, 0.0, None),
            named("New", new_kid, 2.0, Some(0)),
        ])
        .unwrap();
        assert!(refresh_copies(&mut scene, &mut load(&changed)));
        assert!(
            !scene
                .entities
                .iter()
                .any(|e| e.name.as_deref() == Some("Kid"))
        );
        let added = find(&scene, "New");
        assert_eq!(added.parent, Some(0));
        assert_ne!(added.object_id.as_deref(), Some(new_kid));
        assert_eq!(added.prefab.as_ref().unwrap().member, new_kid);
    }

    #[test]
    fn a_deleted_member_stays_deleted_and_extra_children_stay() {
        let base = prefab();
        let mut scene = placed(&base);
        let kid_id = find(&scene, "Kid").object_id.clone().unwrap();
        scene.remove_objects(&HashSet::from([kid_id]));
        scene.entities.push(named(
            "Mine",
            "00000000-0000-4000-8000-000000000009",
            0.0,
            Some(0),
        ));
        stamp_overrides(&mut scene, &mut load(&base));
        refresh_copies(&mut scene, &mut load(&base));
        assert!(
            !scene
                .entities
                .iter()
                .any(|e| e.name.as_deref() == Some("Kid"))
        );
        assert_eq!(find(&scene, "Mine").parent, Some(0));
        assert!(copy_changes(&scene, 0).iter().any(|c| c == "Mine: added"));
    }

    #[test]
    fn a_missing_prefab_leaves_a_copy_alone() {
        let base = prefab();
        let mut scene = placed(&base);
        find_mut(&mut scene, "Kid").disabled = true;
        let before = scene.clone();
        let mut missing = |_: &str| None;
        stamp_overrides(&mut scene, &mut missing);
        assert!(!refresh_copies(&mut scene, &mut missing));
        assert_eq!(scene, before);
    }

    #[test]
    fn references_between_members_are_compared_and_copied_in_the_right_ids() {
        let mut top = named("Top", TOP, 0.0, None);
        top.rules = vec![RuleData::new(
            RuleTrigger::SceneStart,
            RuleAction::Destroy { target: KID.into() },
        )];
        let base = Prefab::from_entities(vec![top, named("Kid", KID, 1.0, Some(0))]).unwrap();
        let mut scene = placed(&base);
        let kid_id = find(&scene, "Kid").object_id.clone().unwrap();
        // A copy points at its own kid, and that is not a change.
        assert!(matches!(
            &find(&scene, "Top").rules[0].action,
            RuleAction::Destroy { target } if *target == kid_id
        ));
        stamp_overrides(&mut scene, &mut load(&base));
        assert!(
            find(&scene, "Top")
                .prefab
                .as_ref()
                .unwrap()
                .overrides
                .is_empty()
        );
        assert!(!refresh_copies(&mut scene, &mut load(&base)));
    }

    #[test]
    fn apply_makes_this_copy_the_prefab_and_other_copies_follow() {
        let base = prefab();
        let mut scene = placed(&base);
        let other = base.copy_entities(Some(PATH));
        let offset = scene.entities.len();
        scene.entities.extend(other.into_iter().map(|mut entity| {
            entity.parent = entity.parent.map(|parent| parent + offset);
            entity.name = entity.name.map(|name| format!("Other {name}"));
            entity
        }));
        stamp_overrides(&mut scene, &mut load(&base));
        // The other copy's names are its own overrides; undo them to follow.
        scene.entities[offset].name = Some("Top".into());
        scene.entities[offset + 1].name = Some("Kid".into());
        stamp_overrides(&mut scene, &mut load(&base));

        scene.entities[1].disabled = true;
        scene.entities.push(named(
            "Extra",
            "00000000-0000-4000-8000-000000000008",
            3.0,
            Some(0),
        ));
        let applied = apply_copy(&mut scene, 0, &base).unwrap();
        assert_eq!(applied.entity().transform, base.entity().transform);
        let names: Vec<_> = applied
            .entities()
            .map(|e| e.name.clone().unwrap())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"Extra".to_owned()));
        assert!(applied.entities().all(|e| e.prefab.is_none()));

        assert!(refresh_copies(&mut scene, &mut load(&applied)));
        // Three members each now.
        assert_eq!(scene.entities.len(), 6);
        assert!(scene.entities[offset + 1].disabled);
        assert_eq!(scene.entities[0].transform, at(50.0));
    }

    #[test]
    fn revert_restores_the_prefab_and_removes_extras() {
        let base = prefab();
        let mut scene = placed(&base);
        find_mut(&mut scene, "Kid").disabled = true;
        scene.entities.push(named(
            "Extra",
            "00000000-0000-4000-8000-000000000008",
            3.0,
            Some(0),
        ));
        stamp_overrides(&mut scene, &mut load(&base));
        revert_copy(&mut scene, 0);
        refresh_copies(&mut scene, &mut load(&base));
        assert!(!find(&scene, "Kid").disabled);
        assert_eq!(scene.entities.len(), 2);
        assert_eq!(find(&scene, "Top").transform, at(50.0));
    }

    #[test]
    fn a_prefab_from_before_links_still_links() {
        let old = Prefab::new(SceneEntity {
            name: Some("Old".into()),
            ..Default::default()
        });
        let mut scene = Scene {
            entities: old.copy_entities(Some(PATH)),
            ..Default::default()
        };
        assert_eq!(scene.entities[0].prefab.as_ref().unwrap().member, "#0");
        stamp_overrides(&mut scene, &mut load(&old));
        scene.entities[0].disabled = true;
        let applied = apply_copy(&mut scene, 0, &old).unwrap();
        assert!(applied.entity().object_id.is_none());
        assert!(applied.entity().disabled);
    }

    #[test]
    fn a_child_finds_its_copys_top_object() {
        let base = prefab();
        let scene = placed(&base);
        let kid_id = find(&scene, "Kid").object_id.clone().unwrap();
        assert_eq!(copy_root_of(&scene, &kid_id, &mut load(&base)), Some(0));
        assert_eq!(copy_root_of(&scene, "nobody", &mut load(&base)), None);
    }
}
