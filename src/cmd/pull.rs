//! `forge pull` — fetch the workspace's live deploy state from its forge-git
//! remote. The mirror of `forge push`: forge-git is push-to-deploy, and this
//! pulls the current desired-state tree back so you can inspect it or rebase
//! onto it. The remote history is server-derived and usually DIVERGES from your
//! local commits, so a plain fast-forward often won't apply — `--ff` opts into
//! it and fails loudly when it can't, pointing you at the fetched ref to rebase.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::client::ForgeClient;
use crate::cmd::push::{current_branch, inject_token, redact, remote_url};

#[derive(Debug, clap::Args)]
pub struct PullArgs {
    /// Git remote to fetch from — the forge-git remote `forge init` sets up.
    #[arg(long, default_value = "forge")]
    pub remote: String,
    /// Branch to fetch. Defaults to the current branch.
    #[arg(long)]
    pub branch: Option<String>,
    /// After fetching, fast-forward the current branch to it. Fails (loudly)
    /// when it isn't a fast-forward — forge-git history usually diverges.
    #[arg(long)]
    pub ff: bool,
    /// Repo root (the git working tree). Defaults to the current directory.
    #[arg(long)]
    pub manifest_dir: Option<PathBuf>,
}

pub async fn run(args: PullArgs, client: &ForgeClient) -> Result<()> {
    let dir = args
        .manifest_dir
        .as_deref()
        .unwrap_or_else(|| Path::new("."));
    let branch = match args.branch.as_deref() {
        Some(b) => b.to_string(),
        None => current_branch(dir)?,
    };
    let url = remote_url(dir, &args.remote).with_context(|| {
        format!(
            "resolving remote '{}' — add it with \
             `git remote add {} https://git.forge.run/<ws-id>/<repo>`",
            args.remote, args.remote
        )
    })?;
    let authed = inject_token(&url, &client.token())?;

    // Fetch <branch> and update refs/remotes/<remote>/<branch>. The authed URL
    // (bearer as `x-token:<token>@`) is passed to the git subprocess only and
    // never printed; stderr is redacted on failure.
    eprintln!("pull: git fetch {} {branch} …", args.remote);
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{}/{branch}", args.remote);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["fetch", &authed, &refspec])
        .output()
        .context("spawning git fetch")?;
    if !out.status.success() {
        let err = redact(&String::from_utf8_lossy(&out.stderr).replace(&authed, &url));
        bail!("git fetch from {url} failed:\n{}", err.trim());
    }
    eprintln!("pull: ✅ fetched {}/{branch}", args.remote);

    if args.ff {
        let refname = format!("refs/remotes/{}/{branch}", args.remote);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["merge", "--ff-only", &refname])
            .output()
            .context("spawning git merge")?;
        if !out.status.success() {
            bail!(
                "fast-forward failed — forge-git history usually diverges from local; \
                 rebase onto {refname} instead:\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        eprintln!("pull: fast-forwarded {branch}");
    }
    Ok(())
}
