//! Configuration resolution: home config + project config + env + flags.
//!
//! The "active config" is just `(base_url, token)` for the v0 surface.
//! Profiles are a thin layer on top: a name → `(base_url, token)` map
//! in `~/.forge/config.toml`, picked by `--profile` or
//! `default_profile`.
//!
//! `forge login` is the only command that resolves base_url + profile
//! *without* a token (because login is what produces the token); see
//! [`resolve_for_login`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Fully-resolved config the rest of the CLI sees. Field-level
/// optionality is squashed here — by the time we hand this off to a
/// command, both fields must be present or we've already errored.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub base_url: String,
    pub token: String,
}

/// Layered config from disk. Loaded once, merged into `ResolvedConfig`.
/// Same struct serializes back out: `forge login` writes through
/// `save_profile_token` which round-trips this shape.
#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    /// Profile name to use when none is set on the command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    default_profile: Option<String>,
    /// Per-named-workspace settings.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    profile: HashMap<String, ProfileEntry>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProfileEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// Refresh token saved alongside the access token. Currently
    /// unused — `forge refresh` doesn't exist yet — but capturing
    /// it here means a future rotation command finds it without
    /// the user re-running `forge login` to get one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
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

    let profile_entry = chosen_profile.as_ref().and_then(|name| {
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

/// Resolve only what `forge login` needs: a base URL and a profile
/// name. Token is not required (login is what produces it).
/// Profile name defaults to `"default"` when nothing's set, so a
/// fresh user can run `forge login --base-url https://...` without
/// any pre-existing config.
pub fn resolve_for_login(
    cli_base_url: Option<String>,
    cli_profile: Option<String>,
) -> Result<(String, String)> {
    let home = read_optional(home_config_path()?)?;
    let project = read_optional(project_config_path())?;

    let chosen_profile = cli_profile
        .or(project.as_ref().and_then(|c| c.default_profile.clone()))
        .or(home.as_ref().and_then(|c| c.default_profile.clone()))
        .unwrap_or_else(|| "default".to_string());

    let profile_entry = project
        .as_ref()
        .and_then(|c| c.profile.get(&chosen_profile))
        .or_else(|| home.as_ref().and_then(|c| c.profile.get(&chosen_profile)));

    let base_url = cli_base_url
        .or_else(|| profile_entry.and_then(|p| p.base_url.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no base URL set: pass --base-url, set FORGE_BASE_URL, or define one in \
                 ~/.forge/config.toml under the active profile",
            )
        })?;
    Ok((base_url, chosen_profile))
}

/// What `forge logout` reads out of the config so it knows what
/// to revoke + clear. None of these are required to be present —
/// a missing profile is a no-op for logout.
#[derive(Debug, Clone)]
pub struct StoredProfile {
    pub base_url: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

/// Read the saved tokens for `profile` from `~/.forge/config.toml`.
/// Returns `Ok(None)` if the profile doesn't exist or has no
/// tokens yet (e.g. created by `--profile` flag, never logged in).
pub fn read_profile_token(profile: &str) -> Result<Option<StoredProfile>> {
    let path = home_config_path()?;
    let cfg: ConfigFile = match read_optional(path)? {
        Some(c) => c,
        None => return Ok(None),
    };
    let entry = match cfg.profile.get(profile) {
        Some(e) => e,
        None => return Ok(None),
    };
    let (Some(base_url), Some(access_token)) = (entry.base_url.clone(), entry.token.clone()) else {
        return Ok(None);
    };
    Ok(Some(StoredProfile {
        base_url,
        access_token,
        refresh_token: entry.refresh_token.clone(),
    }))
}

/// Remove a profile from `~/.forge/config.toml`. Idempotent — a
/// missing profile is a no-op success.
pub fn clear_profile_token(profile: &str) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = match read_optional(path.clone())? {
        Some(c) => c,
        None => return Ok(()),
    };
    if cfg.profile.remove(profile).is_none() {
        return Ok(());
    }
    // If we just cleared the default profile, drop the
    // default_profile field too — it'd otherwise point at a
    // missing entry.
    if cfg.default_profile.as_deref() == Some(profile) {
        cfg.default_profile = None;
    }
    let serialized = toml::to_string_pretty(&cfg).context("serialize ~/.forge/config.toml")?;
    atomic_write(&path, serialized.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    set_owner_only_permissions(&path);
    Ok(())
}

/// Atomic-write a profile's token + base_url back into
/// `~/.forge/config.toml`. Read-modify-write so other profiles in
/// the same file are preserved untouched. Refuses to clobber the
/// project config (`./.forge.toml`) — that file is committed
/// alongside the customer's repo and shouldn't carry secrets.
pub fn save_profile_token(
    profile: &str,
    base_url: &str,
    access_token: &str,
    refresh_token: Option<&str>,
) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = read_optional(path.clone())?.unwrap_or_default();

    let entry = cfg.profile.entry(profile.to_string()).or_default();
    entry.base_url = Some(base_url.to_string());
    entry.token = Some(access_token.to_string());
    entry.refresh_token = refresh_token.map(str::to_string);

    // First-time init: if no default_profile is set, lock in the one
    // we just wrote. Subsequent `forge login --profile foo` calls
    // don't change the default — explicit user action required.
    if cfg.default_profile.is_none() {
        cfg.default_profile = Some(profile.to_string());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let serialized = toml::to_string_pretty(&cfg).context("serialize ~/.forge/config.toml")?;
    atomic_write(&path, serialized.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    set_owner_only_permissions(&path);
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent", path.display()))?;
    let tmp = dir.join(format!(".forge-config-{}.tmp", std::process::id(),));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) {
    // Best-effort: Windows / WASI / etc. don't expose POSIX modes,
    // and the home-directory ACL is the operative permission anyway.
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
    let bytes =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: ConfigFile =
        toml::from_str(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg))
}
