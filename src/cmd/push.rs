//! `forge push` — push the workspace to its forge-git remote and block until
//! the converge finishes, decoding the silent failure modes into real errors.
//!
//! A raw `git push` returns when desired state is *recorded*, not when the
//! workspace is *live*. So "pushed" reads as "done" when the converge may not
//! have happened at all, and the failures are silent (`in_sync: false`,
//! `live_hash` unmoved, `last_error: null`). This command waits for
//! `reconcile/status` to reach `in_sync && live_hash == desired_hash`, and on
//! anything else translates the state into an actionable message:
//!
//! - live_hash never advances, no error → UNSTAGED components (did
//!   `forge wasm-upload` run?); the runtime now names the module.
//! - `stuck` / `last_error` → printed verbatim (destructive schema delta,
//!   hash mismatch, route collision); the converge fails closed.
//! - reconcile loop disabled + no progress → triggers `reconcile/now` once.
//!
//! Exit code is non-zero on stuck/error/timeout, so CI gates on real
//! convergence, not on the push returning.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::client::ForgeClient;

#[derive(Debug, clap::Args)]
pub struct PushArgs {
    /// Git remote to push to — the forge-git remote `forge init` sets up.
    #[arg(long, default_value = "forge")]
    pub remote: String,
    /// Branch to push. Defaults to the current branch.
    #[arg(long)]
    pub branch: Option<String>,
    /// Push and return immediately; don't wait for the converge.
    #[arg(long)]
    pub no_wait: bool,
    /// Seconds to wait for convergence before failing. Default 300.
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// Repo root (the git working tree). Defaults to the current directory.
    #[arg(long)]
    pub manifest_dir: Option<PathBuf>,
}

pub async fn run(args: PushArgs, client: &ForgeClient) -> Result<()> {
    push_and_poll(
        client,
        args.manifest_dir.as_deref(),
        &args.remote,
        args.branch.as_deref(),
        args.no_wait,
        args.timeout,
    )
    .await
}

/// Shared entry so `forge ship` can compose push after build + upload.
pub async fn push_and_poll(
    client: &ForgeClient,
    dir: Option<&Path>,
    remote: &str,
    branch: Option<&str>,
    no_wait: bool,
    timeout_secs: u64,
) -> Result<()> {
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let branch = match branch {
        Some(b) => b.to_string(),
        None => current_branch(dir)?,
    };
    let url = remote_url(dir, remote).with_context(|| {
        format!(
            "resolving remote '{remote}' — add it with \
             `git remote add {remote} https://git.forge.run/<ws-id>/<repo>`"
        )
    })?;
    let authed = inject_token(&url, &client.token())?;

    // Auto-commit a dirty `forge.lock` before pushing. `forge build` stamps
    // built-byte hashes into the lock in the WORKING TREE; pushing the
    // pre-stamp HEAD ships a tree whose lock is missing or stale, and the
    // server refuses to converge it ('pushed a workspace.json with no
    // committed forge.lock'). Every fresh-tree `forge ship` hit this — the
    // build succeeded, the push succeeded, and the converge silently never
    // happened. Scoped to forge.lock only: user source stays untouched.
    let lock_dirty = Command::new("git")
        .args(["status", "--porcelain", "--", "forge.lock"])
        .current_dir(dir)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if lock_dirty {
        eprintln!("push: committing stamped forge.lock");
        let add = Command::new("git")
            .args(["add", "forge.lock"])
            .current_dir(dir)
            .status()?;
        if !add.success() {
            bail!("git add forge.lock failed");
        }
        let commit = Command::new("git")
            .args([
                "-c",
                "user.email=forge-ship@forge.run",
                "-c",
                "user.name=forge ship",
                "commit",
                "-q",
                "-m",
                "forge ship: stamp built-byte lock hashes",
                "--",
                "forge.lock",
            ])
            .current_dir(dir)
            .status()?;
        if !commit.success() {
            bail!("git commit forge.lock failed");
        }
    }
    let pushed_sha = head_sha(dir)?;

    // Snapshot the pre-push commit so we can tell when the server has actually
    // moved to OUR push (robust even when the forge-git repo's history differs
    // from the local repo's, e.g. a source-mirror deploy).
    let before_sha = fetch_status(client)
        .await
        .map(|s| s.git_sha)
        .unwrap_or_default();

    eprintln!("push: git push {remote} {branch} …");
    git_push(dir, &authed, &url, &branch)?;

    if no_wait {
        eprintln!(
            "push: recorded (--no-wait). Convergence runs in the background — \
             poll reconcile/status."
        );
        return Ok(());
    }
    poll_until_converged(
        client,
        &pushed_sha,
        &before_sha,
        Duration::from_secs(timeout_secs),
    )
    .await?;

    // Validation #59 — post-converge surface smoke gate. The deploy is live;
    // verify the declared landing hosts actually route host-first and don't
    // bounce off to another surface's host (the app→code redirect outage).
    smoke_check_surfaces(dir).await?;
    Ok(())
}

/// After convergence, GET the root `/` of each declared landing host and assert
/// it does NOT redirect to a DIFFERENT host. A cross-host root bounce is the
/// signature of a broken/absent surface config (`app.forge.run/` → `code`).
/// Definite bounces fail the deploy loudly; unreachable hosts warn and are
/// skipped (never fail a good deploy on a transient network/DNS blip).
async fn smoke_check_surfaces(dir: &Path) -> Result<()> {
    let Some(domains) = crate::cmd::surface_lint::agreed_domains(dir) else {
        return Ok(()); // no surface config → nothing to smoke-test.
    };
    let Some(hosts) = domains.get("hosts").and_then(|h| h.as_object()) else {
        return Ok(());
    };
    // Landing hosts = those serving an interactive `app` or public `marketing`
    // surface; their root is a user entry point that must stay on-host.
    let landing: Vec<String> = hosts
        .iter()
        .filter(|(_, p)| {
            p.get("allowed_surfaces")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .any(|s| s.as_str() == Some("app") || s.as_str() == Some("marketing"))
                })
                .unwrap_or(false)
        })
        .map(|(h, _)| h.clone())
        .collect();
    if landing.is_empty() {
        return Ok(());
    }

    let http = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("push: ⚠ smoke gate skipped — HTTP client init failed: {e}");
            return Ok(());
        }
    };

    let mut bounces = Vec::new();
    for host in &landing {
        let url = format!("https://{host}/");
        match http.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_redirection() {
                    let loc = resp
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    let dest_host = reqwest::Url::parse(loc)
                        .ok()
                        .and_then(|u| u.host_str().map(|s| s.to_string()));
                    match dest_host {
                        Some(dh) if &dh != host => {
                            bounces.push(format!(
                                "  ✗ {host}/ → {} {} (bounced OFF-HOST to `{dh}`)",
                                status.as_u16(),
                                loc
                            ));
                        }
                        _ => eprintln!("push:   ✓ {host}/ → {} (stays on host)", status.as_u16()),
                    }
                } else {
                    eprintln!("push:   ✓ {host}/ → {} (serves in place)", status.as_u16());
                }
            }
            Err(e) => eprintln!("push:   ⚠ {host}/ unreachable, smoke check skipped: {e}"),
        }
    }

    if !bounces.is_empty() {
        bail!(
            "push: ✗ SURFACE SMOKE GATE FAILED — {} landing host(s) bounce off-host at root:\n{}\n\
             The deploy is live but host routing is broken (the surface config is absent or wrong). \
             Check each app.json `domains` block and roll forward with a fix.",
            bounces.len(),
            bounces.join("\n"),
        );
    }
    eprintln!(
        "push: ✅ surface smoke gate passed ({} landing host(s) route on-host)",
        landing.len()
    );
    Ok(())
}

/// `#[serde(default)]` covers an ABSENT field but not an explicit `null` —
/// and a freshly-provisioned workspace's reconcile/status returns
/// `{"git_sha": null, "desired_hash": null, "live_hash": null, ...}` until
/// its first converge lands. Decoding that as `String` aborted the CLI's
/// converge wait on exactly the workspaces the wait exists for.
fn null_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::Deserialize as _;
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(serde::Deserialize, Default, Clone)]
struct Reconcile {
    #[serde(default, deserialize_with = "null_string")]
    git_sha: String,
    #[serde(default, deserialize_with = "null_string")]
    desired_hash: String,
    #[serde(default, deserialize_with = "null_string")]
    live_hash: String,
    #[serde(default)]
    in_sync: bool,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    stuck: bool,
    #[serde(default)]
    reconcile_loop_enabled: bool,
}

async fn fetch_status(client: &ForgeClient) -> Result<Reconcile> {
    client
        .get_json("/api/v1/manage/reconcile/status")
        .await
        .map_err(|e| anyhow::anyhow!("reconcile/status: {e}"))
}

async fn poll_until_converged(
    client: &ForgeClient,
    pushed_sha: &str,
    before_sha: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let short = |s: &str| s.chars().take(10).collect::<String>();
    let mut last_live = String::new();
    let mut stale = 0u32;
    let mut nudged = false;
    eprintln!(
        "push: waiting for converge (git_sha {})…",
        short(pushed_sha)
    );

    loop {
        let st = fetch_status(client).await?;

        if st.stuck || st.last_error.is_some() {
            bail!(
                "converge FAILED CLOSED (the previous version keeps serving): {}",
                st.last_error.unwrap_or_else(|| "stuck".into())
            );
        }

        // Converged: in sync, live == desired, AND the server is on our push
        // (matches our SHA, or has advanced past the pre-push commit).
        let is_ours = st.git_sha == pushed_sha
            || (!before_sha.is_empty() && st.git_sha != before_sha)
            || before_sha.is_empty();
        if st.in_sync && st.live_hash == st.desired_hash && is_ours {
            eprintln!("push: ✅ converged @ {}", short(&st.git_sha));
            return Ok(());
        }

        // No-progress detection.
        if st.live_hash == last_live {
            stale += 1;
        } else {
            stale = 0;
            last_live = st.live_hash.clone();
        }
        // If the reconcile loop is off and nothing's moving, nudge it once.
        if stale >= 2 && !nudged {
            nudged = true;
            if !st.reconcile_loop_enabled {
                eprintln!(
                    "push: reconcile loop disabled + no progress — triggering reconcile/now …"
                );
                let _: std::result::Result<serde_json::Value, _> = client
                    .post_json("/api/v1/manage/reconcile/now", &serde_json::json!({}))
                    .await;
            }
        }

        if Instant::now() >= deadline {
            let hint = if !pushed_sha.is_empty()
                && st.git_sha != pushed_sha
                && st.git_sha == before_sha
            {
                "the server never recorded your push — check the remote/branch (nothing new arrived).".to_string()
            } else if !st.reconcile_loop_enabled {
                "the reconcile loop is disabled on this node — recorded but not auto-converging."
                    .to_string()
            } else {
                "live_hash never advanced with no error — the classic symptom of UNSTAGED \
                 components. Did `forge wasm-upload` run before the push?"
                    .to_string()
            };
            bail!(
                "timed out after {}s (in_sync={}, git_sha={}, live!=desired). {}",
                timeout.as_secs(),
                st.in_sync,
                short(&st.git_sha),
                hint
            );
        }
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
}

// ── git helpers (shell out; no git2 dependency) ─────────────────────────

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("spawning git")
}

fn git_str(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git(dir, args)?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) fn current_branch(dir: &Path) -> Result<String> {
    git_str(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}
fn head_sha(dir: &Path) -> Result<String> {
    git_str(dir, &["rev-parse", "HEAD"])
}
pub(crate) fn remote_url(dir: &Path, remote: &str) -> Result<String> {
    git_str(dir, &["remote", "get-url", remote])
}

fn git_push(dir: &Path, authed_url: &str, safe_url: &str, branch: &str) -> Result<()> {
    // The authed URL carries the bearer as `x-token:<token>@` and is passed to
    // the git subprocess only (local). Never printed; stderr is redacted.
    //
    // `--no-thin` is REQUIRED, not an optimisation toggle. git (default, and
    // aggressively so on >= 2.54) sends a THIN pack: deltas whose base objects
    // it believes the remote already has are omitted. forge-git completes a
    // thin pack against its own object store, but on a COLD deploy (a fresh
    // workspace repo — every CI runner, every first ship) that store is EMPTY,
    // so an omitted base has nowhere to resolve and forge-git rejects the push
    // with `missing-necessary-objects` after ingesting the rest. A complete
    // (non-thin) pack is self-contained and always accepted. git 2.50 happened
    // to send a complete pack here and worked; 2.54's thinner packing exposed
    // the latent assumption. Deploy pushes are small — the size cost of
    // non-thin is negligible against never failing a first deploy.
    let out = git(dir, &["push", "--no-thin", authed_url, branch])?;
    if !out.status.success() {
        let err = redact(&String::from_utf8_lossy(&out.stderr).replace(authed_url, safe_url));
        bail!("git push to {safe_url} failed:\n{}", err.trim());
    }
    Ok(())
}

/// Insert `x-token:<token>@` after the scheme, replacing any existing userinfo.
pub(crate) fn inject_token(url: &str, token: &str) -> Result<String> {
    let (scheme, rest) = url.split_once("://").context("remote URL missing scheme")?;
    let host_path = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
    Ok(format!("{scheme}://x-token:{token}@{host_path}"))
}

/// Strip a leaked `x-token:<...>@` from a string.
pub(crate) fn redact(s: &str) -> String {
    let needle = "x-token:";
    if let Some(i) = s.find(needle)
        && let Some(at) = s[i..].find('@')
    {
        return format!("{}x-token:***@{}", &s[..i], &s[i + at + 1..]);
    }
    s.to_string()
}
