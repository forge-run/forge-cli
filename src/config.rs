//! Configuration resolution: home config + project config + env + flags.
//!
//! Two operating modes share one TOML file at `~/.forge/config.toml`:
//!
//! 1. **Federated mode** (the default for `forge login`
//!    against app.forge.run). Auth state lives in `[portal]` (the
//!    portal session — `fr_u_*` bearer + refresh + email) and
//!    `[current]` (the operator's selected `tenant_id` +
//!    `workspace_id`). Per-workspace `fr_f_*` bearers are cached
//!    under `[cache.workspaces."ws-XXXXXXXX"]` keyed by id; the
//!    HTTP client transparently re-mints + refreshes the cache on
//!    401.
//!
//! 2. **Direct mode** (the legacy break-glass path). Auth state
//!    lives in `[profile.<name>]` exactly as before — `base_url` +
//!    `token` + `refresh_token`. `forge login --workspace-url …`
//!    writes here; other commands resolve through it when no
//!    `[portal]` section is present. The 401-retry path is
//!    disabled in this mode (the CLI has no portal to mint from).
//!
//! Detection of which mode applies is automatic: the resolver
//! prefers federated when `[portal]` + `[current].workspace_id`
//! are both set, falls back to direct mode otherwise.
//!
//! `config_version` tracks shape evolution: v1 is the original
//! profile-only file (still readable; auto-detected as direct
//! mode). v2 is this shape with `[portal]` + `[current]` + cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Fully-resolved config the rest of the CLI sees. Field-level
/// optionality is squashed here — by the time we hand this off to a
/// command, the load-bearing fields must be present or we've already
/// errored.
///
/// `portal` is `Some` when the CLI is operating in federated mode and
/// the HTTP client should enable the 401-retry interceptor: on a 401,
/// mint a fresh `fr_f_*` via `portal.base_url` → consume against the
/// target workspace → cache the result under
/// `[cache.workspaces.<workspace_id>]` → retry the original request
/// once. `portal: None` means direct mode (no retry; this is the
/// legacy break-glass behaviour).
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub base_url: String,
    pub token: String,
    pub portal: Option<PortalSession>,
    pub workspace_id: Option<String>,
}

/// Portal session — the operator's user-tier bearer against the
/// portal workspace (typically app.forge.run). Carried alongside
/// every command's per-workspace config so the 401-retry path can
/// drive `/api/v1/auth/sso/issue` without re-prompting.
#[derive(Debug, Clone)]
pub struct PortalSession {
    pub base_url: String,
    pub bearer: String,
    #[allow(dead_code)] // surfaced into config on save; refresh isn't wired yet
    pub refresh_token: Option<String>,
    #[allow(dead_code)]
    pub email: Option<String>,
}

/// Layered config from disk. Loaded once, merged into `ResolvedConfig`.
/// Same struct serializes back out: every writer round-trips through
/// here so unknown sections (a future-version's keys, or another tool's
/// data) survive untouched.
#[derive(Debug, Default, Deserialize, Serialize)]
struct ConfigFile {
    /// File-format version. Old files (no field) are v1; new
    /// writers set v2. Stays in lockstep with the schema-bumping
    /// changes here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_version: Option<u32>,
    /// Profile name to use when none is set on the command line.
    /// Direct-mode only; federated mode keys off `[current]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    default_profile: Option<String>,
    /// Per-named-workspace settings (direct mode).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    profile: HashMap<String, ProfileEntry>,
    /// Federated mode — portal session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portal: Option<PortalEntry>,
    /// Federated mode — operator's currently-selected tenant +
    /// workspace, written by `forge tenant use` / `forge ws use`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<CurrentEntry>,
    /// Federated mode — per-workspace federated bearer cache.
    /// Keyed on `workspace_id`; refreshed on 401 by the client's
    /// retry interceptor.
    #[serde(default, skip_serializing_if = "CacheBlock::is_empty")]
    cache: CacheBlock,
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

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
struct PortalEntry {
    base_url: String,
    bearer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
struct CurrentEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct CacheBlock {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    workspaces: HashMap<String, WorkspaceCacheEntry>,
}

impl CacheBlock {
    fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
struct WorkspaceCacheEntry {
    api_url: String,
    bearer: String,
    /// RFC 3339 timestamp. The client doesn't validate it client-
    /// side (just attaches the bearer); on the server's expiry the
    /// 401-retry path catches it.
    expires_at: String,
}

/// Resolve the active config, layering home > project > env > flags.
/// `cli_*` arguments are the values from the top-level CLI flags
/// (already env-fallbacked by clap when set).
///
/// Federated mode is preferred when `[portal]` is configured:
/// - With `[current].workspace_id` + a cache hit → return the
///   cached `fr_f_*` (the client may still hit 401 on expiry and
///   re-mint).
/// - With `[current].workspace_id` but no cache → return the
///   portal session and an empty placeholder bearer; the first
///   request will 401 and the client will mint.
/// - Without `[current].workspace_id` → require `--base-url`
///   and `--token` (operator running a one-shot against a
///   specific workspace before `forge ws use`).
///
/// Direct mode runs when `[portal]` is absent — the legacy
/// behaviour.
pub fn resolve(
    cli_base_url: Option<String>,
    cli_token: Option<String>,
    cli_profile: Option<String>,
) -> Result<ResolvedConfig> {
    let home = read_optional(home_config_path()?)?;
    let project = read_optional(project_config_path())?;

    // Federated mode first: the home config holds the portal
    // session, project config can't override it (the portal is
    // user-laptop state, not repo state).
    let portal_entry = home.as_ref().and_then(|c| c.portal.as_ref()).cloned();
    let current_entry = home.as_ref().and_then(|c| c.current.as_ref()).cloned();

    if let Some(portal) = portal_entry {
        let portal_session = PortalSession {
            base_url: portal.base_url.trim_end_matches('/').to_string(),
            bearer: portal.bearer,
            refresh_token: portal.refresh_token,
            email: portal.email,
        };
        // Workspace selection: --base-url + --token override
        // [current] selection; otherwise read [current].workspace_id.
        let selected_ws = cli_base_url
            .as_ref()
            .and(cli_token.as_ref())
            .map(|_| None) // both flags present → treat as direct override
            .unwrap_or_else(|| current_entry.as_ref().and_then(|c| c.workspace_id.clone()));

        if let (Some(bu), Some(tok)) = (cli_base_url.clone(), cli_token.clone()) {
            // Explicit override — use the supplied bearer but still
            // pass the portal session along so the client can refresh
            // if the explicit bearer 401s and the operator happens to
            // be targeting [current].workspace_id.
            return Ok(ResolvedConfig {
                base_url: bu.trim_end_matches('/').to_string(),
                token: tok,
                portal: Some(portal_session),
                workspace_id: current_entry.as_ref().and_then(|c| c.workspace_id.clone()),
            });
        }

        let workspace_id = selected_ws.ok_or_else(|| {
            anyhow::anyhow!(
                "federated mode: no workspace selected. Run `forge ws use <workspace_id>` or pass \
             both --base-url and --token to override.",
            )
        })?;
        let cache_entry = home
            .as_ref()
            .and_then(|c| c.cache.workspaces.get(&workspace_id))
            .cloned();
        let (base_url, token) = match cache_entry {
            Some(e) => (e.api_url.trim_end_matches('/').to_string(), e.bearer),
            None => {
                // No cache yet — derive the conventional URL and
                // let the 401-retry mint on first request. Empty
                // token works because every request will 401.
                let derived = format!("https://{workspace_id}.forge.run");
                (derived, String::new())
            }
        };
        return Ok(ResolvedConfig {
            base_url,
            token,
            portal: Some(portal_session),
            workspace_id: Some(workspace_id),
        });
    }

    // Direct mode (legacy). Pick the profile to read defaults
    // from. Project config wins for default_profile so a repo can
    // pin its environment without touching the user's home config.
    let chosen_profile = cli_profile
        .or_else(|| project.as_ref().and_then(|c| c.default_profile.clone()))
        .or_else(|| home.as_ref().and_then(|c| c.default_profile.clone()));

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
                 ~/.forge/config.toml or ./.forge.toml under the active profile. \
                 For federated access via the portal, run `forge login --portal-url …`.",
            )
        })?;
    let token = cli_token
        .or_else(|| profile_entry.and_then(|p| p.token.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no token set: pass --token, set FORGE_TOKEN, or define one in \
                 ~/.forge/config.toml or ./.forge.toml under the active profile.",
            )
        })?;
    Ok(ResolvedConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        token,
        portal: None,
        workspace_id: None,
    })
}

/// Resolve only what `forge login` needs: a base URL and a profile
/// name (for direct mode), OR a portal URL (for federated mode).
/// Token is not required (login is what produces it). Profile name
/// defaults to `"default"` for direct mode.
pub fn resolve_for_login(
    cli_base_url: Option<String>,
    cli_profile: Option<String>,
) -> Result<(String, String)> {
    let home = read_optional(home_config_path()?)?;
    let project = read_optional(project_config_path())?;

    let chosen_profile = cli_profile
        .or_else(|| project.as_ref().and_then(|c| c.default_profile.clone()))
        .or_else(|| home.as_ref().and_then(|c| c.default_profile.clone()))
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
                 ~/.forge/config.toml under the active profile.",
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
    write_config(&path, &cfg)
}

/// Save a profile entry (direct mode). Atomic-write so other
/// profiles / federated state stays intact.
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

    if cfg.default_profile.is_none() {
        cfg.default_profile = Some(profile.to_string());
    }
    cfg.config_version = Some(cfg.config_version.unwrap_or(2));
    write_config(&path, &cfg)
}

/// Save the portal session (federated mode). Writes to `[portal]`
/// alongside any existing direct-mode profiles; doesn't touch
/// `[current]` or `[cache.workspaces.…]`.
pub fn save_portal_session(
    base_url: &str,
    bearer: &str,
    refresh_token: Option<&str>,
    email: Option<&str>,
) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = read_optional(path.clone())?.unwrap_or_default();
    cfg.portal = Some(PortalEntry {
        base_url: base_url.trim_end_matches('/').to_string(),
        bearer: bearer.to_string(),
        refresh_token: refresh_token.map(str::to_string),
        email: email.map(str::to_string),
    });
    cfg.config_version = Some(2);
    write_config(&path, &cfg)
}

/// Read the saved portal session, if any. Used by `forge logout`
/// to revoke the portal bearer + by `forge sso connect` /
/// `forge tenant *` / `forge ws *` to know where the portal is.
pub fn read_portal_session() -> Result<Option<PortalSession>> {
    let path = home_config_path()?;
    let cfg: ConfigFile = match read_optional(path)? {
        Some(c) => c,
        None => return Ok(None),
    };
    let p = match cfg.portal {
        Some(p) => p,
        None => return Ok(None),
    };
    Ok(Some(PortalSession {
        base_url: p.base_url,
        bearer: p.bearer,
        refresh_token: p.refresh_token,
        email: p.email,
    }))
}

/// Clear the portal session + `[current]` selection. Cache stays
/// (the fr_f_* bearers are short-lived anyway and will expire on
/// the runtime side; clearing them is logout-as-cleanup, not
/// logout-as-security).
pub fn clear_portal_session() -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = match read_optional(path.clone())? {
        Some(c) => c,
        None => return Ok(()),
    };
    cfg.portal = None;
    cfg.current = None;
    cfg.cache = CacheBlock::default();
    write_config(&path, &cfg)
}

/// Set the active tenant. Federated mode only — direct mode
/// callers should set `--base-url` / `--token` instead.
pub fn set_current_tenant(tenant_id: &str) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = read_optional(path.clone())?.unwrap_or_default();
    let current = cfg.current.get_or_insert_with(CurrentEntry::default);
    current.tenant_id = Some(tenant_id.to_string());
    cfg.config_version = Some(2);
    write_config(&path, &cfg)
}

/// Set the active workspace. Federated mode only. Changing the
/// active workspace also clears the previous one's cache entry —
/// the cached bearer is workspace-scoped, so keeping stale entries
/// just bloats the file.
pub fn set_current_workspace(workspace_id: &str) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = read_optional(path.clone())?.unwrap_or_default();
    let current = cfg.current.get_or_insert_with(CurrentEntry::default);
    current.workspace_id = Some(workspace_id.to_string());
    cfg.config_version = Some(2);
    write_config(&path, &cfg)
}

/// Read the active tenant + workspace selection, if any.
pub fn read_current_selection() -> Result<(Option<String>, Option<String>)> {
    let path = home_config_path()?;
    let cfg: ConfigFile = match read_optional(path)? {
        Some(c) => c,
        None => return Ok((None, None)),
    };
    Ok(match cfg.current {
        Some(c) => (c.tenant_id, c.workspace_id),
        None => (None, None),
    })
}

/// Cache a freshly-minted federated bearer for `workspace_id`.
/// Called by the client's 401-retry interceptor after a successful
/// mint→consume dance.
pub fn cache_workspace_bearer(
    workspace_id: &str,
    api_url: &str,
    bearer: &str,
    expires_at: &str,
) -> Result<()> {
    let path = home_config_path()?;
    let mut cfg: ConfigFile = read_optional(path.clone())?.unwrap_or_default();
    cfg.cache.workspaces.insert(
        workspace_id.to_string(),
        WorkspaceCacheEntry {
            api_url: api_url.trim_end_matches('/').to_string(),
            bearer: bearer.to_string(),
            expires_at: expires_at.to_string(),
        },
    );
    cfg.config_version = Some(2);
    write_config(&path, &cfg)
}

/// Detect "your config predates federated mode" — printed by the
/// new subcommands (`forge tenant`, `forge ws`, `forge sso`) when
/// the user is on an old config and the operator-helpful thing is
/// "re-run login against the portal," not "try harder."
///
/// Returns true when at least one direct-mode profile exists but
/// no `[portal]` section does.
pub fn is_legacy_config() -> Result<bool> {
    let path = home_config_path()?;
    let cfg: ConfigFile = match read_optional(path)? {
        Some(c) => c,
        None => return Ok(false),
    };
    Ok(cfg.portal.is_none() && !cfg.profile.is_empty())
}

fn write_config(path: &Path, cfg: &ConfigFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let serialized = toml::to_string_pretty(cfg).context("serialize ~/.forge/config.toml")?;
    atomic_write(path, serialized.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    set_owner_only_permissions(path);
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
fn set_owner_only_permissions(_path: &Path) {}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Old-shape (v1) configs without `[portal]` still parse — the
    /// federated additions are all `#[serde(default)]` / Optional.
    #[test]
    fn old_v1_config_parses_unchanged() {
        let toml_str = r#"
default_profile = "default"

[profile.default]
base_url = "https://ws-abc.forge.run"
token = "fr_u_xyz"
"#;
        let cfg: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("default"));
        assert!(cfg.portal.is_none());
        assert!(cfg.current.is_none());
        assert!(cfg.cache.workspaces.is_empty());
        assert_eq!(cfg.profile.len(), 1);
    }

    /// New-shape (v2) configs round-trip every section.
    #[test]
    fn new_v2_config_round_trips() {
        let toml_str = r#"
config_version = 2

[portal]
base_url = "https://app.forge.run"
bearer = "fr_u_portal"
refresh_token = "fr_s_portal"
email = "ops@forge.run"

[current]
tenant_id = "0b8971ba-7954-49b8-8159-f960a20925a3"
workspace_id = "ws-30bb36de"

[cache.workspaces."ws-30bb36de"]
api_url = "https://ws-30bb36de.forge.run"
bearer = "fr_f_cached"
expires_at = "2026-05-16T08:00:00Z"
"#;
        let cfg: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.config_version, Some(2));
        assert_eq!(cfg.portal.as_ref().unwrap().bearer.as_str(), "fr_u_portal");
        assert_eq!(
            cfg.current.as_ref().unwrap().workspace_id.as_deref(),
            Some("ws-30bb36de")
        );
        assert_eq!(cfg.cache.workspaces.len(), 1);
        let cached = cfg.cache.workspaces.get("ws-30bb36de").unwrap();
        assert_eq!(cached.bearer, "fr_f_cached");
        // Round-trip preserves shape.
        let back = toml::to_string_pretty(&cfg).unwrap();
        let again: ConfigFile = toml::from_str(&back).unwrap();
        assert_eq!(again.config_version, Some(2));
        assert_eq!(again.portal.unwrap().bearer, "fr_u_portal");
    }

    /// Configs with both direct profiles AND federated `[portal]`
    /// section parse cleanly — the migration story is "add
    /// `[portal]` next to existing `[profile.…]` blocks, no
    /// deletion required."
    #[test]
    fn mixed_legacy_and_federated_config_parses() {
        let toml_str = r#"
config_version = 2
default_profile = "scratch"

[portal]
base_url = "https://app.forge.run"
bearer = "fr_u_portal"

[profile.scratch]
base_url = "https://ws-old.forge.run"
token = "fr_u_legacy"
"#;
        let cfg: ConfigFile = toml::from_str(toml_str).unwrap();
        assert!(cfg.portal.is_some());
        assert!(cfg.profile.contains_key("scratch"));
    }
}
