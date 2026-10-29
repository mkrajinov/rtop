//! The field catalog in `assets/explain.toml`, embedded at compile time.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Entry {
    pub title: String,
    pub what: String,
    pub linux: String,
    pub macos: String,
    #[serde(default)]
    pub pitfalls: Vec<String>,
    #[serde(default, rename = "try")]
    pub try_it: Vec<String>,
    #[serde(default)]
    pub man: Vec<String>,
}

pub struct Catalog(HashMap<String, Entry>);

impl Catalog {
    pub fn load() -> Self {
        let raw = include_str!("../assets/explain.toml");
        Catalog(toml::from_str(raw).expect("assets/explain.toml is valid (checked by tests)"))
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.0.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every `Field::new("key", ...)` in the source and every key the UI uses
    /// must have a catalog entry, otherwise the explain panel would be empty.
    #[test]
    fn every_used_key_is_documented() {
        let cat = Catalog::load();
        let mut missing = Vec::new();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(dir).unwrap() {
                let path = e.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.ends_with("explain.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for chunk in text.split("Field::new(").skip(1) {
                    let chunk = chunk.trim_start();
                    if let Some(rest) = chunk.strip_prefix('"') {
                        let key = &rest[..rest.find('"').unwrap()];
                        if cat.get(key).is_none() {
                            missing.push(format!("{key} ({})", path.display()));
                        }
                    }
                }
            }
        }
        for key in crate::ui::EXPLAIN_KEYS.iter().chain(crate::app::KUBE_LESSONS).chain(&["pod", "container"]) {
            if cat.get(key).is_none() {
                missing.push(format!("{key} (ui)"));
            }
        }
        assert!(missing.is_empty(), "undocumented keys: {missing:?}");
    }
}
