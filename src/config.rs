//! Configuration resolution: home config + project config + env + flags.
//!
//! The "active config" is just `(base_url, token)` for the v0 surface.
//! Profiles are a thin layer on top: a name → `(base_url, token)` map
//! in `~/.forge/config.toml`, picked by `--profile` or
//! `default_profile`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Fully-resolved config the rest of the CLI sees. Field-level
/// optionality is squashed here — by the time we hand this off to a
/// command, both fields must be present or we've already errored.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub base_url: String,
    pub token: String,
}

/// Layered config from disk. Loaded once, merged into `ResolvedConfig`.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    /// Profile name to use when none is set on the command line.
    default_profile: Option<String>,
    /// Per-named-workspace settings.
    #[serde(default)]
    profile: std::collections::HashMap<String, ProfileEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileEntry {
    base_url: Option<String>,
    token: Option<String>,
}

/// Resolve the active config, layering home > project > env > flags.
/// `cli_*` arguments are the values from the top-level CLI flags
/// (already env-fallbacked by clap when set).
pub fn resolve(
    cli_base_url: Option<String>,
    cli_token: Option<String>,
    cli_profile: Option<String>,
) -> Result<ResolvedConfig> {
    let home = read_optional(home_config_path()?)?;
    let project = read_optional(project_config_path())?;

    // Pick the profile to read defaults from. Project config wins for
    // default_profile so a repo can pin its environment without
    // touching the user's home config.
    let chosen_profile = cli_profile
        .or(project.as_ref().and_then(|c| c.default_profile.clone()))
        .or(home.as_ref().and_then(|c| c.default_profile.clone()));

    let profile_entry = chosen_profile
        .as_ref()
        .and_then(|name| {
            project
                .as_ref()
                .and_then(|c| c.profile.get(name))
                .or_else(|| home.as_ref().and_then(|c| c.profile.get(name)))
        });

    let base_url = cli_base_url
        .or_else(|| profile_entry.and_then(|p| p.base_url.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no base URL set: pass --base-url, set FORGE_BASE_URL, or define one in \
                 ~/.forge/config.toml or ./.forge.toml under the active profile",
            )
        })?;
    let token = cli_token
        .or_else(|| profile_entry.and_then(|p| p.token.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no token set: pass --token, set FORGE_TOKEN, or define one in \
                 ~/.forge/config.toml or ./.forge.toml under the active profile",
            )
        })?;

    Ok(ResolvedConfig { base_url, token })
}

fn home_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("$HOME is unset; cannot locate ~/.forge/config.toml"))?;
    Ok(PathBuf::from(home).join(".forge").join("config.toml"))
}

fn project_config_path() -> PathBuf {
    PathBuf::from(".forge.toml")
}

fn read_optional(path: PathBuf) -> Result<Option<ConfigFile>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let cfg: ConfigFile = toml::from_str(&bytes)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg))
}
