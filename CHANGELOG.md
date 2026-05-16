# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This file mirrors the GitHub release notes; consult the release page
for downloadable binaries and platform-specific tarballs.

## [Unreleased]

## [0.2.2] - 2026-05-16

### Fixed

- `forge update` was failing with
  `Could not find the required path in the archive: "forge"` because
  the release tarball wraps the binary inside a
  `forge-<tag>-<target>/` directory and `self_update` was looking
  for it at the archive root. Tell `self_update` the in-archive
  path explicitly.
- `forge update` no longer prompts `Do you want to continue? [Y/n]`
  on the swap step. The operator already invoked `forge update`,
  which is consent enough; the previous prompt blocked
  non-interactive invocations (CI, scripted updates).

## [0.2.1] - 2026-05-16

### Added

- `SECURITY.md` — vulnerability-disclosure pointer.
- `CHANGELOG.md` (this file).
- README quickstart expanded with a full subcommand reference.

### Fixed

- `install.sh` now strips the `com.apple.quarantine` xattr on macOS
  after installation, so first-run no longer triggers the Gatekeeper
  "cannot be opened because the developer cannot be verified" dialog.
- `install.sh` reports a clear error if the installed binary fails to
  run (Gatekeeper / missing libc / platform mismatch), instead of the
  previous silent empty `installed ` line.

## [0.2.0] - 2026-05-16

The first **public** release. Source is now MIT-OR-Apache-2.0 dual
licensed and binaries are published on GitHub Releases for the four
common platforms.

### Added

- **Portal-mediated authentication.** `forge login` runs the device
  flow against `https://app.forge.run` and saves a portal session to
  `~/.forge/config.toml`. Subsequent commands target a workspace via
  `forge tenant use <id>` + `forge ws use <id>`; the CLI mints a
  short-lived per-workspace bearer on demand via the portal's SSO
  mint/consume path.
- **Transparent re-auth on 401.** When a workspace bearer expires
  mid-request, the HTTP client transparently re-mints via the portal
  and retries the original call once. The cached bearer is written
  through to `~/.forge/config.toml` so subsequent invocations reuse
  it until expiry.
- **`forge sso`** — operator-facing entry point for the same flow.
  `forge sso login` runs the portal device flow; `forge sso connect
  <workspace_id>` mints a federated bearer explicitly (useful in
  shell scripts that pre-fetch a bearer for downstream tools).
- **`forge tenant {list,use}`** — list tenants the portal user
  belongs to, set the active one.
- **`forge ws {list,use}`** — list workspaces in the active tenant,
  set the active one. Subsequent commands target it.
- **`forge update`** — self-update from GitHub releases. `--check`
  reports whether a newer version is available; `--force` re-installs
  the current version. sha256-verified, atomic-swap.
- **`forge login --no-browser`** — opt out of the automatic
  browser-opening on the device-flow step. Useful for CI runners and
  headless boxes.
- **Cross-platform release pipeline.** `.github/workflows/release.yml`
  builds + ships tarballs for macOS aarch64, macOS x86_64,
  Linux aarch64, Linux x86_64 on every `v*` tag. sha256 checksums
  published alongside.
- **One-liner installer.** `curl -sSfL …/install.sh | sh` detects
  platform, downloads the matching tarball, verifies sha256, drops
  the binary at `~/.local/bin/forge`. Honors `FORGE_INSTALL_DIR` and
  `FORGE_CLI_VERSION`.
- **Project templates vendored** into `templates/` (`echo`,
  `mcp-tool`, `subscription-publisher`) so `forge new --template
  <name>` works for any builder, including `cargo install --git`
  users.

### Changed

- **`forge login` default behaviour** — federated mode is now the
  default. `forge login` with no flags delegates to `forge sso login`
  against `https://app.forge.run`. Direct-mode workspace-local login
  remains available behind `--base-url` as a break-glass.
- **`forge logout`** clears the portal session and per-workspace
  bearer cache in addition to the legacy profile.

### Deprecated

- Direct-mode `forge login --base-url <url>` (the legacy
  workspace-local device flow). Still works as a break-glass for
  unreachable-portal scenarios; emits a one-line stderr deprecation
  warning when invoked.

### Removed

- Nothing — direct-mode auth + legacy `[profile.*]` config sections
  remain readable for back-compat.

## [0.1.0] - earlier

Pre-public versions. See `git log` for granular history.

[Unreleased]: https://github.com/forge-run/forge-cli/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/forge-run/forge-cli/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/forge-run/forge-cli/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/forge-run/forge-cli/releases/tag/v0.2.0
