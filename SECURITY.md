# Security policy

## Reporting a vulnerability

Please report vulnerabilities **privately** — do not open a public issue.

Email: **security@forge.run**

Include:

- A description of the issue and its impact (what an attacker can do).
- Steps to reproduce, ideally with a minimal example.
- The `forge --version` output and the platform you observed it on.
- Whether the issue affects only forge-cli, or also the platform server-side surface.

We aim to acknowledge reports within 2 business days and ship a fix or
mitigation within 14 days for high-impact issues. We will credit reporters
in the release notes unless asked not to.

## Scope

This repository covers the **forge-cli** binary and its installer.
In-scope concerns include but aren't limited to:

- Credential leakage (the CLI handles bearer tokens; any path that
  logs or echoes them is a bug).
- Token-handling errors that could weaken auth posture (e.g. failing
  open on validation errors, accepting bearers past their expiry).
- Installer integrity (the `install.sh` curl-pipe-sh path; sha256
  verification correctness; tarball-spoofing windows).
- Self-update (`forge update`) integrity — same concerns as the
  installer.
- Local config-file safety (mode `0600` on `~/.forge/config.toml`;
  no secrets written to logs / project configs).

The platform server-side (forge-runtime, forge-control-plane, the
storage layer) is out of scope here; report platform issues to the
same address with a clear note. We'll route internally.

## Out of scope

- Bugs in upstream crates (we'll forward to maintainers but won't
  ourselves issue a CVE for them).
- Issues that require physical access to the operator's machine or
  control of their browser session.
- Denial-of-service against the operator's own laptop (the CLI is
  client-side; if you can DoS your own CLI you can also just not
  run it).

## Versions

Security fixes target the **latest released version** on the `main`
branch. We do not backport to older tags; upgrade with `forge update`
or reinstall from the install one-liner.
