//! Persistent user settings in `$XDG_CONFIG_HOME/rtop/config.toml`
//! (default `~/.config/rtop/config.toml`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Colour theme, built from Tailwind palette values (see `rtop --help`).
    /// `gray` is accepted for configs written by older versions.
    #[serde(alias = "gray")]
    pub theme: Option<String>,
    /// Tailwind accent hue (red … rose).
    pub accent: Option<String>,
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("rtop").join("config.toml"))
}

/// A missing or unreadable file means defaults; rtop must start regardless.
pub fn load() -> Config {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(cfg: &Config) -> anyhow::Result<PathBuf> {
    let p = path().ok_or_else(|| anyhow::anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, toml::to_string(cfg)?)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cfg = Config { theme: Some("stone".into()), accent: Some("emerald".into()) };
        let back: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.theme.as_deref(), Some("stone"));
        assert_eq!(back.accent.as_deref(), Some("emerald"));
        assert!(toml::from_str::<Config>("").unwrap().theme.is_none());
        let old: Config = toml::from_str("gray = \"zinc\"").unwrap();
        assert_eq!(old.theme.as_deref(), Some("zinc"));
    }
}
