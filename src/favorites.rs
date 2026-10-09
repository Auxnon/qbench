//! Favorite tables, persisted per connection in `<config dir>/qbench/favorites.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::db::TableRef;

/// Each favorite gets a number key (1–9, 0), so the list stops at ten.
pub const MAX_FAVORITES: usize = 10;

#[derive(Default, Serialize, Deserialize)]
struct Store {
    connections: BTreeMap<String, Vec<TableRef>>,
}

pub struct Favorites {
    key: String,
    path: Option<PathBuf>,
    store: Store,
}

impl Favorites {
    pub fn load(key: String) -> Self {
        let path = dirs::config_dir().map(|d| d.join("qbench").join("favorites.json"));
        let store = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { key, path, store }
    }

    pub fn list(&self) -> &[TableRef] {
        self.store.connections.get(&self.key).map_or(&[], Vec::as_slice)
    }

    pub fn contains(&self, t: &TableRef) -> bool {
        self.list().contains(t)
    }

    /// Adds or removes `t`; returns whether it is now a favorite.
    pub fn toggle(&mut self, t: &TableRef) -> Result<bool, String> {
        let list = self.store.connections.entry(self.key.clone()).or_default();
        let added = if let Some(i) = list.iter().position(|x| x == t) {
            list.remove(i);
            false
        } else if list.len() >= MAX_FAVORITES {
            return Err(format!("favorites are full ({MAX_FAVORITES}); remove one first"));
        } else {
            list.push(t.clone());
            true
        };
        self.save()?;
        Ok(added)
    }

    pub fn remove(&mut self, idx: usize) -> Result<(), String> {
        if let Some(list) = self.store.connections.get_mut(&self.key)
            && idx < list.len()
        {
            list.remove(idx);
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, serde_json::to_string_pretty(&self.store)?)
        };
        write().map_err(|e| format!("saving favorites: {e}"))
    }
}

/// Number key shown for the favorite at `idx` (1..9 then 0).
pub fn shortcut(idx: usize) -> char {
    char::from_digit(((idx + 1) % 10) as u32, 10).unwrap_or('?')
}

/// Favorite index selected by a number key.
pub fn index_for_digit(c: char) -> Option<usize> {
    c.to_digit(10).map(|d| if d == 0 { 9 } else { d as usize - 1 })
}
