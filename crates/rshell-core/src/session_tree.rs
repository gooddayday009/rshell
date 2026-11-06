//! Persistent ordering and validated moves for the saved-session tree.
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Item {
    Folder(Uuid),
    Profile(Uuid),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Folder, Profile};

    fn fixture() -> (Config, [Item; 5]) {
        let a = Folder {
            id: Uuid::new_v4(),
            name: "A".into(),
            parent_id: None,
        };
        let b = Folder {
            id: Uuid::new_v4(),
            name: "B".into(),
            parent_id: None,
        };
        let child = Folder {
            id: Uuid::new_v4(),
            name: "child".into(),
            parent_id: Some(a.id),
        };
        let p = Profile {
            name: "first".into(),
            host: "localhost".into(),
            ..Default::default()
        };
        let q = Profile {
            name: "second".into(),
            host: "localhost".into(),
            ..Default::default()
        };
        let items = [
            Item::Folder(a.id),
            Item::Folder(b.id),
            Item::Folder(child.id),
            Item::Profile(p.id),
            Item::Profile(q.id),
        ];
        (
            Config {
                folders: vec![b, child, a],
                profiles: vec![q, p],
                ..Default::default()
            },
            items,
        )
    }

    #[test]
    fn legacy_order_mixed_reordering_and_reload() {
        let (config, [a, b, _, p, q]) = fixture();
        let mut legacy = serde_json::to_value(&config).unwrap();
        legacy.as_object_mut().unwrap().remove("session_order");
        let config: Config = serde_json::from_value(legacy).unwrap();
        assert_eq!(config.session_children(None), vec![a, b, p, q]);
        let config = config
            .moved_session(b, Placement::Before(a))
            .unwrap()
            .unwrap();
        let config = config
            .moved_session(q, Placement::Before(p))
            .unwrap()
            .unwrap();
        let config = config
            .moved_session(p, Placement::Before(b))
            .unwrap()
            .unwrap();
        assert_eq!(config.session_children(None), vec![p, b, a, q]);
        let config = config
            .moved_session(b, Placement::After(q))
            .unwrap()
            .unwrap();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let temp = tempfile::tempdir_in(root).unwrap();
        let path = temp.path().join("config.json");
        config.save(&path).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().session_children(None),
            vec![p, a, q, b]
        );
    }

    #[test]
    fn moving_subtrees_preserves_children_and_places_filtered_neighbors() {
        let (config, [a, b, child, p, q]) = fixture();
        let Item::Folder(a_id) = a else {
            unreachable!()
        };
        let Item::Folder(b_id) = b else {
            unreachable!()
        };
        let config = config
            .moved_session(p, Placement::Into(Some(a_id)))
            .unwrap()
            .unwrap();
        let config = config
            .moved_session(a, Placement::Into(Some(b_id)))
            .unwrap()
            .unwrap();
        assert_eq!(config.session_children(Some(a_id)), vec![child, p]);
        assert_eq!(config.session_children(Some(b_id)), vec![a]);
        // Hidden siblings are retained: the UI submits an anchor, never a filtered index.
        let config = config
            .moved_session(q, Placement::Before(p))
            .unwrap()
            .unwrap();
        assert_eq!(config.session_children(Some(a_id)), vec![child, q, p]);
        let config = config
            .moved_session(a, Placement::Into(None))
            .unwrap()
            .unwrap();
        assert_eq!(config.session_children(None), vec![b, a]);
        assert_eq!(config.session_children(Some(a_id)), vec![child, q, p]);
        let config = config
            .moved_session(p, Placement::Into(None))
            .unwrap()
            .unwrap();
        assert_eq!(config.session_children(None), vec![b, a, p]);
    }

    #[test]
    fn invalid_moves_and_no_ops_leave_source_unchanged() {
        let (mut config, [a, b, child, p, q]) = fixture();
        let Item::Folder(a_id) = a else {
            unreachable!()
        };
        let Item::Folder(child_id) = child else {
            unreachable!()
        };
        config
            .folders
            .iter_mut()
            .find(|f| {
                Some(f.id)
                    == match b {
                        Item::Folder(id) => Some(id),
                        _ => None,
                    }
            })
            .unwrap()
            .name = "child".into();
        let before = serde_json::to_value(&config).unwrap();
        for (item, destination) in [
            (a, Placement::Into(Some(a_id))),
            (a, Placement::Into(Some(child_id))),
            (a, Placement::Before(child)),
            (b, Placement::Into(Some(a_id))),
            (p, Placement::Into(Some(Uuid::new_v4()))),
            (p, Placement::After(Item::Profile(Uuid::new_v4()))),
        ] {
            assert!(config.moved_session(item, destination).is_err());
        }
        assert!(
            config
                .moved_session(p, Placement::Before(p))
                .unwrap()
                .is_none()
        );
        assert!(
            config
                .moved_session(p, Placement::Before(q))
                .unwrap()
                .is_none()
        );
        assert!(
            config
                .moved_session(q, Placement::Into(None))
                .unwrap()
                .is_none()
        );
        assert_eq!(serde_json::to_value(&config).unwrap(), before);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Into(Option<Uuid>),
    Before(Item),
    After(Item),
}

impl Config {
    pub fn session_parent(&self, item: Item) -> Result<Option<Uuid>> {
        match item {
            Item::Folder(id) => self
                .folders
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.parent_id),
            Item::Profile(id) => self
                .profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.folder_id),
        }
        .context("文件夹或会话已不存在")
    }

    pub fn session_children(&self, parent: Option<Uuid>) -> Vec<Item> {
        let ranks: HashMap<_, _> = self
            .session_order
            .iter()
            .enumerate()
            .map(|(i, v)| (*v, i))
            .collect();
        let mut children: Vec<_> = self
            .folders
            .iter()
            .filter(|f| f.parent_id == parent)
            .map(|f| (Item::Folder(f.id), 0, &f.name))
            .chain(
                self.profiles
                    .iter()
                    .filter(|p| p.folder_id == parent)
                    .map(|p| (Item::Profile(p.id), 1, &p.name)),
            )
            .collect();
        children.sort_by(|a, b| {
            ranks
                .get(&a.0)
                .unwrap_or(&usize::MAX)
                .cmp(ranks.get(&b.0).unwrap_or(&usize::MAX))
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(b.2))
        });
        children.into_iter().map(|v| v.0).collect()
    }

    /// The original stays untouched on invalid moves, including descendant cycles.
    pub fn moved_session(&self, item: Item, placement: Placement) -> Result<Option<Self>> {
        let source = self.session_parent(item)?;
        let destination = match placement {
            Placement::Into(parent) => {
                if let Some(id) = parent {
                    self.session_parent(Item::Folder(id))?;
                }
                parent
            }
            Placement::Before(target) | Placement::After(target) => {
                if target == item {
                    return Ok(None);
                }
                self.session_parent(target)?
            }
        };
        ensure!(
            !destination.is_some_and(|id| item == Item::Folder(id)),
            "不能将文件夹移入自身"
        );
        let previous = self.session_children(destination);
        let mut siblings = previous.clone();
        siblings.retain(|v| *v != item);
        let index = match placement {
            Placement::Into(_) => siblings.len(),
            Placement::Before(target) | Placement::After(target) => {
                siblings
                    .iter()
                    .position(|v| *v == target)
                    .context("目标已不存在")?
                    + usize::from(matches!(placement, Placement::After(_)))
            }
        };
        siblings.insert(index, item);
        if source == destination && previous == siblings {
            return Ok(None);
        }
        let mut candidate = self.clone();
        match item {
            Item::Folder(id) => {
                candidate
                    .folders
                    .iter_mut()
                    .find(|f| f.id == id)
                    .unwrap()
                    .parent_id = destination
            }
            Item::Profile(id) => {
                candidate
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == id)
                    .unwrap()
                    .folder_id = destination
            }
        }
        candidate.validate_folders()?;
        let changed: HashSet<_> = siblings.iter().copied().collect();
        candidate.session_order.retain(|v| !changed.contains(v));
        candidate.session_order.extend(siblings);
        Ok(Some(candidate))
    }
}
