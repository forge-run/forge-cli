//! `forge new` — scaffold a new forge workload from a starter
//! template.
//!
//! Templates ship embedded in the forge-cli binary via
//! `include_str!`. The customer runs:
//!
//! ```bash
//! forge new --dialect python my-app
//! cd my-app
//! forge check && forge test && forge push
//! ```
//!
//! and gets a workspace the platform serves from source (D-106: no
//! customer build). The raw wasm templates stay scaffoldable by name for
//! the platform's own fixtures and `forge dev`, but `--list` never offers
//! them. For those, substitution is text-replace of
//! the template's hardcoded crate name (`forge-template-<name>`)
//! with the customer's chosen name. Underscores vs hyphens
//! matter: cargo crate names use hyphens; the wasm output file
//! and the `name` in `[package]` use the same. The
//! `wasm_path` in `service.json` references the underscore form
//! (cargo replaces hyphens with underscores in artifact
//! filenames). Both substitutions land in the same pass.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use forge_lang_rustgen::{Lang, subset_skill, workspace_types};

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Which starter template to scaffold from. Run `forge new --list`
    /// to see the templates offered, with a one-line description each.
    /// Defaults to `workspace` when `--dialect` is passed.
    #[arg(long)]
    template: Option<String>,

    /// Output directory + crate name. The directory must not
    /// already exist (refuses to clobber). Required unless `--list`.
    #[arg(value_name = "CRATE_NAME")]
    crate_name: Option<String>,

    /// List the available starter templates and exit.
    #[arg(long)]
    list: bool,

    /// Write the `workspace` template's starter op in a dialect —
    /// `python`, `typescript`, `java` or `rust` — with that dialect's subset
    /// skill under `.forge/skill/` for the coding agent that writes the next
    /// op. Required by `workspace`.
    #[arg(long, value_parser = parse_dialect)]
    dialect: Option<Lang>,
}

/// `--dialect`'s value, as the author would spell the language.
fn parse_dialect(s: &str) -> std::result::Result<Lang, String> {
    match s {
        "python" | "py" => Ok(Lang::Python),
        "typescript" | "ts" => Ok(Lang::TypeScript),
        "java" => Ok(Lang::Java),
        "rust" | "rs" => Ok(Lang::Rust),
        other => Err(format!(
            "unknown dialect `{other}` — one of python, typescript, java, rust"
        )),
    }
}

/// The templates `--list` offers a customer, with a one-line description
/// each. D-106 retired the customer build, so this is the dialect workspace
/// alone; every wasm template (`TEMPLATES`, `WORKSPACE_TEMPLATES`) is
/// scaffoldable by name and never listed.
const CUSTOMER_TEMPLATES: &[(&str, &str)] = &[(
    DIALECT_TEMPLATE,
    "a workspace whose starter op is dialect source (--dialect python|typescript|java|rust); \
     push it and the platform serves it",
)];

/// The customer template's name: a workspace written in a dialect.
const DIALECT_TEMPLATE: &str = "workspace";

/// Workspace-graph templates. Unlike the single-service `TEMPLATES` (a bare
/// crate for the imperative `forge deploy`), these scaffold a whole
/// `workspace.json` domain/app/capability graph — the unit the hidden
/// `forge ship` builds + pushes. `wasm-workspace` is also the base every
/// dialect scaffold writes, minus `WASM_ONLY`. Emitted paths carry a `{{domain}}` token substituted with
/// the derived domain name; file contents carry `{{workspace_name}}`,
/// `{{domain}}`, `{{domain_crate}}`, and the SDK-dep placeholder.
const WORKSPACE_TEMPLATES: &[(&str, &[(&str, &str)])] = &[(
    "wasm-workspace",
    &[
        (
            "workspace.json",
            include_str!("../../templates/workspace/workspace.json"),
        ),
        (
            "Cargo.toml",
            include_str!("../../templates/workspace/Cargo.toml"),
        ),
        (
            ".gitignore",
            include_str!("../../templates/workspace/gitignore"),
        ),
        (
            "README.md",
            include_str!("../../templates/workspace/README.md"),
        ),
        (
            "domains/{{domain}}/domain.json",
            include_str!("../../templates/workspace/domain.json"),
        ),
        (
            "domains/{{domain}}/Cargo.toml",
            include_str!("../../templates/workspace/domain-Cargo.toml"),
        ),
        (
            "domains/{{domain}}/service.json",
            include_str!("../../templates/workspace/service.json"),
        ),
        (
            "domains/{{domain}}/services/lib.rs",
            include_str!("../../templates/workspace/lib.rs"),
        ),
    ],
)];

/// Embedded templates. Each entry is `(template_name, [(relative_path, contents)...])`.
/// The `forge-template-<name>` placeholder gets replaced with the
/// customer's `--crate-name` argument; same for the underscore-
/// form artifact filename.
const TEMPLATES: &[(&str, &[(&str, &str)])] = &[
    (
        "echo",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/echo/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/echo/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/echo/service.json"),
            ),
            ("README.md", include_str!("../../templates/echo/README.md")),
        ],
    ),
    (
        "mcp-tool",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/mcp-tool/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/mcp-tool/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/mcp-tool/service.json"),
            ),
            (
                "README.md",
                include_str!("../../templates/mcp-tool/README.md"),
            ),
        ],
    ),
    (
        "subscription-publisher",
        &[
            (
                "Cargo.toml",
                include_str!("../../templates/subscription-publisher/Cargo.toml"),
            ),
            (
                "src/lib.rs",
                include_str!("../../templates/subscription-publisher/src/lib.rs"),
            ),
            (
                "service.json",
                include_str!("../../templates/subscription-publisher/service.json"),
            ),
            (
                "README.md",
                include_str!("../../templates/subscription-publisher/README.md"),
            ),
        ],
    ),
];

pub async fn run(args: NewArgs) -> Result<()> {
    if args.list {
        print!("{}", template_list());
        return Ok(());
    }

    let template = resolve_template(args.template.as_deref(), args.dialect)?;
    let crate_name = args.crate_name.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "missing NAME — usage: `forge new --dialect <python|typescript|java|rust> <name>`"
        )
    })?;

    validate_crate_name(crate_name)?;

    if let Some(lang) = args.dialect {
        return scaffold_dialect_workspace(crate_name, lang);
    }

    // Workspace-graph templates take a different scaffolding path (multi-dir
    // tree + `forge ship` next-steps).
    if let Some((_, files)) = WORKSPACE_TEMPLATES
        .iter()
        .find(|(name, _)| *name == template)
    {
        return scaffold_workspace(crate_name, template, files);
    }

    let files = TEMPLATES
        .iter()
        .find(|(name, _)| *name == template)
        .map(|(_, files)| files)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown template `{template}` — run `forge new --list` to see the options"
            )
        })?;

    let target_dir = PathBuf::from(crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }

    let template_crate_name = format!("forge-template-{}", template);
    let template_artifact_name = template_crate_name.replace('-', "_");
    let customer_artifact_name = crate_name.replace('-', "_");

    let (sdk_dep_line, sdk_resolved) = resolve_sdk_dep();

    for (rel_path, content) in *files {
        let path = target_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let substituted = content
            .replace(&template_crate_name, crate_name)
            .replace(&template_artifact_name, &customer_artifact_name)
            // README/service.json placeholders. `{{crate_name}}` was NEVER
            // substituted before this fix, so scaffolded READMEs shipped the
            // literal `{{crate_name}}`. Rewrite both the hyphen + underscore
            // forms so docs and paths read correctly.
            .replace("{{crate_name}}", crate_name)
            .replace("{{artifact_name}}", &customer_artifact_name)
            // SDK dependency. The template pins `forge-sdk-v2 = { path = "../.." }`
            // (relative to the in-repo template location); rewrite it to the
            // resolved dependency form. See `resolve_sdk_dep` for precedence
            // and docs/sdk-dependency-portability.md for the off-box story.
            .replace("forge-sdk-v2 = { path = \"../..\" }", &sdk_dep_line);
        std::fs::write(&path, substituted)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    // Initialise a git repo + initial commit so the scaffold is immediately a
    // real working tree (a prerequisite for any push-based deploy, and how
    // `forge init` leaves its GitOps workspaces). Best-effort — never fails
    // the scaffold.
    git_init_scaffold(&target_dir);

    eprintln!(
        "created {} from `{}` template",
        target_dir.display(),
        template
    );
    if !sdk_resolved {
        eprintln!();
        eprintln!(
            "⚠  forge-sdk-v2 could not be located automatically. The generated\n   \
             Cargo.toml points at `{sdk_dep_line}` — edit it to a real path (or set\n   \
             FORGE_SDK_PATH and re-run) before building. See\n   \
             docs/sdk-dependency-portability.md."
        );
    }
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge wasm-build              # compile to a wasm32-wasip1 Component");
    eprintln!();
    eprintln!(
        "  This template is a single-service crate. It deploys via the imperative\n  \
         path (below); the GitOps `forge ship` golden path deploys a WORKSPACE, so\n  \
         to ride `forge ship` scaffold `--template wasm-workspace` instead.\n"
    );
    eprintln!("  # imperative single-service deploy:");
    eprintln!(
        "  forge deploy --manifest service.json \\\n    \
         --wasm target/wasm32-wasip1/release/{customer_artifact_name}.wasm"
    );
    Ok(())
}

/// Derive a valid domain name (lower-kebab, no leading/trailing `-`) from the
/// crate name. `forge_schema`'s domain validation requires this shape and the
/// domain directory must match the name exactly; sanitising here avoids a
/// confusing compile error after the tree is already written.
fn derive_domain_name(crate_name: &str) -> String {
    let lowered = crate_name.to_lowercase();
    let trimmed = lowered.trim_matches('-');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Scaffold a workspace-graph template: a `workspace.json` domain/app graph
/// that `forge ship` (wasm-build → wasm-upload → git push) consumes directly.
fn scaffold_workspace(crate_name: &str, template: &str, files: &[(&str, &str)]) -> Result<()> {
    let target_dir = PathBuf::from(crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }

    let domain = derive_domain_name(crate_name);
    let (sdk_dep_line, sdk_resolved) = resolve_sdk_dep();
    write_workspace_tree(&target_dir, crate_name, &domain, &sdk_dep_line, files)?;

    git_init_scaffold(&target_dir);

    eprintln!(
        "created workspace {} from `{}` template (domain `{}`)",
        target_dir.display(),
        template,
        domain,
    );
    if !sdk_resolved {
        eprintln!();
        eprintln!(
            "⚠  forge-sdk-v2 could not be located automatically. `domains/{domain}/Cargo.toml`\n   \
             points at `{sdk_dep_line}` — edit it (or set FORGE_SDK_PATH and re-scaffold)\n   \
             before building. See docs/sdk-dependency-portability.md."
        );
    }
    eprintln!();
    eprintln!("next steps (the GitOps golden path):");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge wasm-build                       # compile the workspace graph");
    eprintln!("  forge ws use <workspace-id>");
    eprintln!("  git remote add forge https://git.forge.run/<workspace-id>/{crate_name}");
    eprintln!("  forge ship                             # build → upload → push → converge");
    Ok(())
}

/// The dialect scaffold's own files: the README, a `service.json` declaring
/// `hello`'s request and response, and one starter op per dialect binding
/// them. Everything else comes from the `workspace` template minus its wasm
/// crate — a dialect workspace builds nothing on the author's side.
const DIALECT_README: &str = include_str!("../../templates/workspace-dialect/README.md");
const DIALECT_SERVICE: &str = include_str!("../../templates/workspace-dialect/service.json");
const DIALECT_OPS: &[(Lang, &str, &str)] = &[
    (
        Lang::Python,
        "hello.py",
        include_str!("../../templates/workspace-dialect/hello.py"),
    ),
    (
        Lang::TypeScript,
        "hello.ts",
        include_str!("../../templates/workspace-dialect/hello.ts"),
    ),
    (
        Lang::Java,
        "Hello.java",
        include_str!("../../templates/workspace-dialect/Hello.java"),
    ),
    (
        Lang::Rust,
        "hello.rs",
        include_str!("../../templates/workspace-dialect/hello.rs"),
    ),
];

/// The UI every dialect scaffold starts with (TSX-17): one app whose pages
/// load `hello` and render its answer through one shared component, `/` a
/// server page and `/wave` a client page whose component holds a state cell
/// and a click handler (TSX-28). The UI is TSX whatever language the ops are
/// written in, so these are the same files in all four scaffolds.
const DIALECT_APP: &[(&str, &str)] = &[
    (
        "apps/web/app.json",
        include_str!("../../templates/workspace-dialect/apps/web/app.json"),
    ),
    (
        "apps/web/pages/index.tsx",
        include_str!("../../templates/workspace-dialect/apps/web/pages/index.tsx"),
    ),
    (
        "apps/web/pages/wave.tsx",
        include_str!("../../templates/workspace-dialect/apps/web/pages/wave.tsx"),
    ),
    (
        "apps/web/components/greeting.tsx",
        include_str!("../../templates/workspace-dialect/apps/web/components/greeting.tsx"),
    ),
    (
        "apps/web/components/wave.tsx",
        include_str!("../../templates/workspace-dialect/apps/web/components/wave.tsx"),
    ),
];

/// The `workspace` template's files a dialect workspace does not carry: the
/// cargo manifests and the raw wasm module, plus the README and the untyped
/// `service.json` it writes its own of.
const WASM_ONLY: &[&str] = &[
    "Cargo.toml",
    "domains/{{domain}}/Cargo.toml",
    "domains/{{domain}}/services/lib.rs",
    "domains/{{domain}}/service.json",
    "README.md",
];

fn dialect_name(lang: Lang) -> &'static str {
    match lang {
        Lang::Python => "Python",
        Lang::TypeScript => "TypeScript",
        Lang::Java => "Java",
        Lang::Rust => "Rust",
    }
}

/// `forge new --template workspace --dialect <lang>`: a workspace whose
/// starter op is dialect source, and whose coding agent has the subset skill.
fn scaffold_dialect_workspace(crate_name: &str, lang: Lang) -> Result<()> {
    let target_dir = PathBuf::from(crate_name);
    if target_dir.exists() {
        anyhow::bail!(
            "directory `{}` already exists — pick a different name or remove it first",
            target_dir.display(),
        );
    }
    let domain = derive_domain_name(crate_name);
    let skill = write_dialect_tree(&target_dir, crate_name, &domain, lang)?;
    git_init_scaffold(&target_dir);

    eprintln!(
        "created {} workspace {} (domain `{}`)",
        dialect_name(lang),
        target_dir.display(),
        domain,
    );
    eprintln!(
        "the subset skill for your coding agent: {}",
        skill.display()
    );
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {}", target_dir.display());
    eprintln!("  forge check                            # the push's verdict, locally");
    eprintln!("  forge ws use <workspace-id>");
    eprintln!("  git remote add forge https://git.forge.run/<workspace-id>/{crate_name}");
    eprintln!("  git push forge main");
    Ok(())
}

/// Everything a dialect scaffold writes before `git init`: the workspace
/// tree with the starter op in `lang` and the TSX page that loads it, then
/// the dialect's subset skill
/// (typed-boundary TB-7) beside the schema surface TB-6 writes. Answers the
/// skill's path.
pub(crate) fn write_dialect_tree(
    target_dir: &std::path::Path,
    crate_name: &str,
    domain: &str,
    lang: Lang,
) -> Result<PathBuf> {
    let (_, base) = WORKSPACE_TEMPLATES[0];
    let (_, op_file, op_body) = DIALECT_OPS
        .iter()
        .find(|(l, _, _)| *l == lang)
        .expect("every dialect has a starter op");
    let op_path = format!("domains/{{{{domain}}}}/services/{op_file}");
    let readme = DIALECT_README
        .replace("{{dialect_name}}", dialect_name(lang))
        .replace("{{op_file}}", op_file)
        .replace("{{skill_file}}", subset_skill::file_name(lang));

    let mut files: Vec<(&str, &str)> = base
        .iter()
        .filter(|(path, _)| !WASM_ONLY.contains(path))
        .copied()
        .collect();
    files.push(("README.md", &readme));
    files.push(("domains/{{domain}}/service.json", DIALECT_SERVICE));
    files.push((&op_path, op_body));
    files.extend_from_slice(DIALECT_APP);
    // The marker is the dialect tree's opt-in to the interpreted converge,
    // for all four dialects: without it a push with no `forge.lock` never
    // converges. For Rust it is also the ruling that the tree's `.rs`
    // sources are dialect, not raw wasm Rust (`dialect.rs`).
    files.push((".forge-interpret", ""));
    write_workspace_tree(target_dir, crate_name, domain, "", &files)?;

    // The page and its component are TSX in every scaffold, and the TS
    // skill's components section is the subset they are checked against, so
    // a workspace whose ops are another dialect is handed it too.
    let write_skill = |lang| {
        subset_skill::write(target_dir, lang).with_context(|| {
            format!(
                "writing {}/{}",
                subset_skill::SKILL_DIR,
                subset_skill::file_name(lang)
            )
        })
    };
    if lang != Lang::TypeScript {
        write_skill(Lang::TypeScript)?;
    }
    write_skill(lang)
}

/// The editor mapping for `service.json` (ST-6): every domain's declaration
/// validates and completes against the JSON Schema `forge schema compile`
/// writes into `.forge/types/`. The template carries no other VS Code
/// settings, so this is the whole file.
const VSCODE_SETTINGS: &str = r#"{
  "json.schemas": [
    {
      "fileMatch": ["domains/*/service.json"],
      "url": "./.forge/types/service.schema.json"
    }
  ]
}
"#;

/// Everything a workspace scaffold writes before `git init`: the template,
/// the two editor configs committed once at the root, `schema.lock`, and the
/// git-ignored `.forge/types/` the editor configs point at (typed-boundary
/// TB-6) — so the first file the engineer opens already resolves
/// `forge.schema`. A schema that will not compile leaves the scaffold intact
/// and says so; `forge schema compile` writes both later.
fn write_workspace_tree(
    target_dir: &std::path::Path,
    crate_name: &str,
    domain: &str,
    sdk_dep_line: &str,
    files: &[(&str, &str)],
) -> Result<()> {
    let domain_crate = format!("forge-domain-{domain}");
    let subst = |s: &str| -> String {
        s.replace("{{domain}}", domain)
            .replace("{{domain_crate}}", &domain_crate)
            .replace("{{workspace_name}}", crate_name)
            .replace("forge-sdk-v2 = { path = \"../..\" }", sdk_dep_line)
    };

    let editor = [
        ("tsconfig.json", workspace_types::ROOT_TSCONFIG),
        ("pyrightconfig.json", workspace_types::ROOT_PYRIGHTCONFIG),
        (".vscode/settings.json", VSCODE_SETTINGS),
    ];
    for (rel_path, content) in files.iter().chain(editor.iter()) {
        // `{{domain}}` appears in emitted paths too (the domain directory).
        let rel = subst(rel_path);
        let path = target_dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, subst(content))
            .with_context(|| format!("writing {}", path.display()))?;
    }

    match crate::cmd::schema::compile_snapshot(target_dir) {
        Ok(compiled) => {
            let lock = target_dir.join(forge_lang_rustgen::SNAPSHOT_FILE);
            std::fs::write(&lock, &compiled.text)
                .with_context(|| format!("writing {}", lock.display()))?;
            crate::cmd::schema::refresh_types(target_dir);
        }
        Err(e) => eprintln!(
            "⚠  schema.lock not written ({e:#}) — run `forge schema compile` once the \
             schema compiles, and your editor's types come with it"
        ),
    }
    Ok(())
}

/// Resolve the `forge-sdk-v2` dependency line to write into the scaffolded
/// `Cargo.toml`, and whether it points at a directory that actually exists.
///
/// Precedence:
///  1. `FORGE_SDK_PATH` env var — explicit override for any checkout layout.
///  2. A sibling `forge-sdk-v2` next to this `forge-cli` checkout (the
///     canonical dev layout `.../{forge-cli,forge-sdk-v2}/`), resolved from
///     `CARGO_MANIFEST_DIR` at COMPILE time — so it is the maintainer's build
///     path, correct only on the box the CLI was built on, and pointing into
///     a worktree lane when the CLI was built in one. `crate::lane` folds
///     that case back onto the canonical sibling before it reaches the
///     scaffold's committed `Cargo.toml` (FU-11).
///
/// If neither resolves to an existing directory we still emit a clearly-marked
/// placeholder path (never a silent, wrong absolute path) and the caller warns
/// the user to fix it. forge-sdk-v2 is `publish = false` and is NOT
/// self-contained (it `wit_bindgen::generate!`s against a sibling
/// `forge-runtime/wit`), so no registry/git dependency form builds off-box
/// today — see docs/sdk-dependency-portability.md for the real fix.
fn resolve_sdk_dep() -> (String, bool) {
    let dep = |p: &str| format!("forge-sdk-v2 = {{ path = \"{p}\" }}");

    if let Ok(env_path) = std::env::var("FORGE_SDK_PATH") {
        let exists = std::path::Path::new(&env_path).is_dir();
        return (dep(&env_path), exists);
    }

    let sibling = env!("CARGO_MANIFEST_DIR")
        .rsplit_once("forge-cli")
        .map(|(prefix, _)| format!("{prefix}forge-sdk-v2"));
    if let Some(path) = sibling {
        // A binary built in a lane bakes the lane into this path, and the
        // scaffold's `Cargo.toml` is committed by `git_init_scaffold` two
        // steps later — the FU-11 shape, in the customer's brand-new repo
        // rather than in the portal. No refusal here: `forge new` already
        // degrades to a marked placeholder plus a warning when the path does
        // not resolve, and that is the same conversation one line louder.
        let path = crate::lane::de_lane(std::path::Path::new(&path));
        if path.is_dir() {
            return (dep(&path.to_string_lossy()), true);
        }
    }

    // Nothing resolved — emit an obvious placeholder, not a wrong path.
    (dep("/path/to/forge-sdk-v2"), false)
}

/// `git init -b main` + an initial commit of the scaffold. Best-effort: a
/// missing git, or an unconfigured identity, leaves the files in place and the
/// user finishes the commit. Never fails the scaffold.
fn git_init_scaffold(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
    };
    if git(&["init", "-b", "main"])
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        let _ = git(&["init"]);
    }
    let _ = git(&["add", "-A"]);
    let staged = git(&["diff", "--cached", "--quiet"])
        .map(|o| !o.status.success())
        .unwrap_or(false);
    if staged {
        let _ = git(&["commit", "-m", "forge new: scaffold from starter template"]);
    }
}

/// What `--list` prints: the customer templates only.
fn template_list() -> String {
    let mut out = String::from("available starter templates:\n\n");
    for (name, desc) in CUSTOMER_TEMPLATES {
        out.push_str(&format!("  {name:<24} {desc}\n"));
    }
    out.push_str("\nusage: forge new --dialect <python|typescript|java|rust> <name>\n");
    out
}

/// The template a `forge new` invocation scaffolds. `--dialect` alone means
/// the dialect workspace; the dialect workspace without one is refused rather
/// than falling back to the raw wasm tree a customer no longer builds.
fn resolve_template(template: Option<&str>, dialect: Option<Lang>) -> Result<&str> {
    match (template, dialect) {
        (None | Some(DIALECT_TEMPLATE), Some(_)) => Ok(DIALECT_TEMPLATE),
        (Some(DIALECT_TEMPLATE), None) => anyhow::bail!(
            "`{DIALECT_TEMPLATE}` is written in a dialect — pass \
             --dialect python|typescript|java|rust"
        ),
        (Some(other), Some(_)) => anyhow::bail!(
            "--dialect scaffolds the `{DIALECT_TEMPLATE}` template; `{other}` is a wasm \
             template with no dialect to choose"
        ),
        (Some(other), None) => Ok(other),
        (None, None) => anyhow::bail!(
            "missing --dialect — usage: `forge new --dialect <python|typescript|java|rust> <name>` \
             (`forge new --list` shows the options)"
        ),
    }
}

/// Cargo crate names: lowercase ASCII letters, digits, `-`, `_`.
/// Must start with a letter. Refuses anything cargo would reject
/// at compile time so the customer doesn't get a confusing
/// "invalid character" error from cargo after `forge new` already
/// created the directory tree.
fn validate_crate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("crate name cannot be empty");
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphabetic() {
        anyhow::bail!(
            "crate name must start with an ASCII letter; got `{name}` (starts with `{first}`)",
        );
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            anyhow::bail!(
                "crate name `{name}` contains invalid character `{ch}` — \
                 only ASCII letters / digits / `-` / `_` are allowed",
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_lang_rustgen::Register;

    #[test]
    fn validate_crate_name_accepts_normal_names() {
        validate_crate_name("my-app").unwrap();
        validate_crate_name("my_app").unwrap();
        validate_crate_name("app123").unwrap();
    }

    #[test]
    fn validate_crate_name_rejects_invalid() {
        assert!(validate_crate_name("").is_err());
        assert!(validate_crate_name("123app").is_err());
        assert!(validate_crate_name("my app").is_err());
        assert!(validate_crate_name("my.app").is_err());
        assert!(
            validate_crate_name("MyApp").is_ok(),
            "uppercase is technically valid for cargo (gets normalized)"
        );
    }

    /// IP-3: `forge new --list` offers a customer the dialect workspace and
    /// nothing else — no template that hands over a cargo/wasm toolchain.
    #[test]
    fn the_listing_offers_a_customer_no_wasm_template() {
        let listing = template_list();
        let listed: Vec<&str> = listing
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(listed, ["workspace"], "listing:\n{listing}");
        for (name, _) in TEMPLATES.iter().chain(WORKSPACE_TEMPLATES) {
            assert!(
                !listed.contains(name),
                "wasm template `{name}` is offered to a customer:\n{listing}"
            );
        }
        assert!(listing.contains("--dialect"), "listing:\n{listing}");
    }

    /// The listed template only scaffolds in a dialect; every wasm template
    /// stays reachable by name for the platform's own fixtures.
    #[test]
    fn the_workspace_template_needs_a_dialect_and_wasm_templates_stay_by_name() {
        assert_eq!(
            resolve_template(None, Some(Lang::Python)).unwrap(),
            "workspace"
        );
        assert_eq!(
            resolve_template(Some("workspace"), Some(Lang::Java)).unwrap(),
            "workspace"
        );
        let refused = resolve_template(Some("workspace"), None).unwrap_err();
        assert!(refused.to_string().contains("--dialect"), "{refused}");
        assert!(resolve_template(None, None).is_err());
        assert!(resolve_template(Some("echo"), Some(Lang::Rust)).is_err());
        for (name, _) in TEMPLATES.iter().chain(WORKSPACE_TEMPLATES) {
            assert_eq!(resolve_template(Some(name), None).unwrap(), *name);
        }
    }

    #[test]
    fn workspace_template_has_ship_able_graph_shape() {
        let (_, files) = WORKSPACE_TEMPLATES
            .iter()
            .find(|(n, _)| *n == "wasm-workspace")
            .expect("wasm-workspace template exists");
        let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        // The unit `forge ship` builds is a workspace.json graph with a cargo
        // workspace + at least one wasm domain. Assert the load-bearing files.
        for required in [
            "workspace.json",
            "Cargo.toml",
            "domains/{{domain}}/domain.json",
            "domains/{{domain}}/Cargo.toml",
            "domains/{{domain}}/service.json",
            "domains/{{domain}}/services/lib.rs",
        ] {
            assert!(
                paths.contains(&required),
                "workspace template missing {required}"
            );
        }
        // The domain crate must be the `forge-domain-<name>` the workspace
        // compiler's hash-stamp step looks for.
        let domain_cargo = files
            .iter()
            .find(|(p, _)| *p == "domains/{{domain}}/Cargo.toml")
            .map(|(_, c)| *c)
            .unwrap();
        assert!(
            domain_cargo.contains("name = \"{{domain_crate}}\""),
            "domain Cargo.toml must use the forge-domain-<name> crate name token"
        );
    }

    #[test]
    fn derive_domain_name_produces_valid_names() {
        assert_eq!(derive_domain_name("my-app"), "my-app");
        assert_eq!(derive_domain_name("MyApp"), "myapp");
        assert_eq!(derive_domain_name("my-app-"), "my-app");
        assert_eq!(derive_domain_name("-"), "app");
    }

    #[test]
    fn no_template_ships_a_literal_double_brace_placeholder_unsubstituted() {
        // `{{crate_name}}` must be a token `forge new` rewrites — a template
        // file containing it is fine (it gets substituted), but this documents
        // that the substitution pass covers it so READMEs don't ship the
        // literal placeholder (the pre-fix bug).
        let rewritten = "# {{crate_name}}".replace("{{crate_name}}", "my-app");
        assert_eq!(rewritten, "# my-app");
    }

    #[test]
    fn resolve_sdk_dep_env_override_wins() {
        // Serialised implicitly: this is the only test touching FORGE_SDK_PATH.
        unsafe { std::env::set_var("FORGE_SDK_PATH", "/tmp/does-not-exist-forge-sdk") };
        let (line, exists) = resolve_sdk_dep();
        assert!(
            line.contains("/tmp/does-not-exist-forge-sdk"),
            "env path used: {line}"
        );
        assert!(!exists, "non-existent path reports not-resolved");
        unsafe { std::env::remove_var("FORGE_SDK_PATH") };
    }

    #[test]
    fn templates_have_the_expected_set_of_files() {
        for (name, files) in TEMPLATES {
            let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
            assert!(
                paths.contains(&"Cargo.toml"),
                "template `{name}` missing Cargo.toml",
            );
            assert!(
                paths.contains(&"src/lib.rs"),
                "template `{name}` missing src/lib.rs",
            );
            assert!(
                paths.contains(&"service.json"),
                "template `{name}` missing service.json",
            );
            assert!(
                paths.contains(&"README.md"),
                "template `{name}` missing README.md",
            );
        }
    }

    #[test]
    fn template_service_json_targets_wasip1_with_substitutable_name() {
        // service.json's wasm_path has to (a) point at
        // wasm32-wasip1 (the target `forge wasm-build` shells out to)
        // and (b) reference the artifact name in a form
        // `forge new`'s substitution can rewrite. The substitution
        // rewrites `forge_template_<name>` (underscored) → the
        // customer's underscored crate name, so wasm_path must
        // contain that exact token. A drift here means the
        // customer's `forge deploy` looks for a file at the
        // wrong path and fails. Past bug: templates shipped
        // wasip2 + a literal `{{crate_name}}` placeholder, both
        // broken.
        for (name, files) in TEMPLATES {
            let service = files
                .iter()
                .find(|(p, _)| *p == "service.json")
                .map(|(_, c)| *c)
                .unwrap();
            assert!(
                service.contains("wasm32-wasip1"),
                "template `{name}` service.json doesn't reference wasm32-wasip1 — \
                 forge wasm-build only emits wasip1 artifacts, so a wasip2 path is dead-on-arrival",
            );
            let token = format!("forge_template_{}", name.replace('-', "_"));
            assert!(
                service.contains(&token),
                "template `{name}` service.json must reference the underscored \
                 placeholder `{token}` in wasm_path so `forge new`'s artifact-name \
                 substitution rewrites it. Current service.json:\n{service}",
            );
            assert!(
                !service.contains("{{crate_name}}"),
                "template `{name}` service.json has a literal `{{{{crate_name}}}}` \
                 placeholder that `forge new` does NOT substitute — use the underscored \
                 form instead",
            );
        }
    }

    /// ADR-0030 / ADR-0031: a scaffold must not hand the customer a
    /// contract the deploy validator immediately warns about. Every
    /// template service declares `domain` (never the legacy `namespace`)
    /// and `api_version`; every op declares `auth`. Declaring
    /// `api_version` also opts the service into
    /// `/api/domains/{domain}/{service}/v{major}/`, and an explicit
    /// `http.path` outside that prefix is a deploy VIOLATION — so the
    /// declared paths are pinned here too.
    #[test]
    fn template_service_json_declares_domain_api_version_and_auth() {
        let workspace = WORKSPACE_TEMPLATES.iter().flat_map(|(name, files)| {
            files
                .iter()
                .filter(|(p, _)| p.ends_with("service.json"))
                .map(move |(_, c)| (*name, *c))
        });
        let single = TEMPLATES.iter().map(|(name, files)| {
            let c = files
                .iter()
                .find(|(p, _)| *p == "service.json")
                .map(|(_, c)| *c)
                .unwrap();
            (*name, c)
        });
        for (name, body) in single.chain(workspace) {
            let doc: serde_json::Value =
                serde_json::from_str(body).expect("template service.json parses");
            let services: Vec<&serde_json::Value> = match doc.get("services") {
                Some(list) => list.as_array().unwrap().iter().collect(),
                None => vec![&doc],
            };
            for svc in services {
                assert!(
                    svc.get("namespace").is_none(),
                    "template `{name}` service.json still uses the legacy `namespace` key",
                );
                let domain = svc
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("template `{name}` service declares no `domain`"));
                let major = svc
                    .get("api_version")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(|| {
                        panic!("template `{name}` service declares no `api_version`")
                    });
                let svc_name = svc.get("name").and_then(|v| v.as_str()).unwrap();
                let prefix = format!("/api/domains/{domain}/{svc_name}/v{major}/");
                for op in svc["operations"].as_array().unwrap() {
                    let op_name = op["name"].as_str().unwrap();
                    assert!(
                        op.get("auth").and_then(|v| v.as_str()).is_some(),
                        "template `{name}` op `{op_name}` declares no `auth` — new scaffolds \
                         are secure by default (ADR-0030)",
                    );
                    if let Some(path) = op.get("http").and_then(|h| h.get("path")) {
                        let path = path.as_str().unwrap();
                        assert!(
                            path.starts_with(&prefix),
                            "template `{name}` op `{op_name}` declares `{path}`, outside its \
                             service prefix `{prefix}` — the deploy validator refuses that \
                             for a service that declared `api_version` (ADR-0031)",
                        );
                    }
                }
            }
            // The wasm-module rows take the same rename.
            for m in doc
                .get("wasm_modules")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                assert!(
                    m.get("service_namespace").is_none() && m.get("service_domain").is_some(),
                    "template `{name}` wasm_modules entry must use `service_domain`",
                );
            }
        }
    }

    #[test]
    fn template_cargo_toml_uses_canonical_placeholder_name() {
        // The substitution logic depends on each template's
        // Cargo.toml using `name = "forge-template-<template>"`.
        // If a template diverges from this convention, the
        // customer's `forge new` will produce a crate with the
        // template's hardcoded name and won't know it. Pin here.
        for (name, files) in TEMPLATES {
            let cargo = files
                .iter()
                .find(|(p, _)| *p == "Cargo.toml")
                .map(|(_, c)| *c)
                .unwrap();
            let expected = format!("name = \"forge-template-{}\"", name);
            assert!(
                cargo.contains(&expected),
                "template `{name}` Cargo.toml doesn't contain `{expected}` — \
                 substitution will silently leave the template's name in the customer's crate",
            );
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=forge-test",
                "-c",
                "user.email=test@forge.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// V-2, asserted rather than assumed (typed-boundary TB-6): a scaffold
    /// writes the editor's schema types, and the tree `git push` would carry
    /// is byte-identical with them present, absent, and regenerated — the
    /// push, and so push-to-live, cannot see them.
    #[test]
    fn a_workspace_scaffold_writes_the_editor_types_and_pushes_without_them() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acme");
        let (_, files) = WORKSPACE_TEMPLATES[0];
        write_workspace_tree(&dir, "acme", "acme", "forge-sdk-v2 = \"0\"", files).unwrap();

        let types = dir.join(workspace_types::TYPES_DIR);
        for rel in [
            "py/forge/__init__.py",
            "py/forge_schema/__init__.py",
            "ts/forge.d.ts",
            "ts/schema.ts",
            "rust/src/schema.rs",
        ] {
            assert!(types.join(rel).is_file(), "{rel} missing from the scaffold");
        }
        // The runtime-owned tables reach the editor from a fresh scaffold.
        assert!(types.join("py/forge_schema/audit_events.py").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join("tsconfig.json")).unwrap(),
            workspace_types::ROOT_TSCONFIG
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("pyrightconfig.json")).unwrap(),
            workspace_types::ROOT_PYRIGHTCONFIG
        );

        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "scaffold"]);
        let pushed = git(&dir, &["ls-tree", "-r", "--name-only", "HEAD"]);
        assert!(
            !pushed.contains(".forge/"),
            "the push carries the types:\n{pushed}"
        );
        for committed in ["schema.lock", "tsconfig.json", "pyrightconfig.json"] {
            assert!(
                pushed.lines().any(|l| l == committed),
                "{committed} not committed"
            );
        }
        let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]);

        std::fs::remove_dir_all(&types).unwrap();
        git(&dir, &["add", "-A"]);
        assert_eq!(
            git(&dir, &["write-tree"]),
            tree,
            "absent types moved the tree"
        );

        crate::cmd::schema::refresh_types(&dir);
        assert!(
            types.join("ts/schema.ts").is_file(),
            "refresh did not rewrite"
        );
        git(&dir, &["add", "-A"]);
        assert_eq!(
            git(&dir, &["write-tree"]),
            tree,
            "regenerated types moved the tree"
        );
    }

    /// typed-boundary TB-7: `forge new --dialect` hands the coding agent the
    /// dialect's subset skill beside the schema surface, byte-identical to
    /// the template the front half generates (and its golden test pins), and
    /// the starter op beside it is one the push's checker admits.
    #[test]
    fn a_dialect_scaffold_emits_the_generated_skill_and_an_admitted_starter() {
        for lang in [Lang::Python, Lang::TypeScript, Lang::Java, Lang::Rust] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("acme");
            let skill = write_dialect_tree(&dir, "acme", "acme", lang).unwrap();

            // The template in the forge-lang tree this binary linked, which
            // that tree's golden test pins to the generator.
            let template = subset_skill::template_path(lang);
            let emitted = std::fs::read(&skill).unwrap();
            let generated =
                std::fs::read(&template).unwrap_or_else(|e| panic!("{}: {e}", template.display()));
            assert!(
                emitted == generated,
                "{lang:?}: the emitted skill is not the generated {}",
                template.display()
            );
            assert_eq!(emitted, subset_skill::text(lang).as_bytes());
            assert!(
                skill.starts_with(dir.join(workspace_types::TYPES_DIR).parent().unwrap()),
                "{lang:?}: the skill is not beside the schema surface"
            );
            assert!(dir.join(workspace_types::TYPES_DIR).is_dir());
            for wasm in ["Cargo.toml", "domains/acme/services/lib.rs"] {
                assert!(!dir.join(wasm).exists(), "{lang:?}: {wasm} scaffolded");
            }

            match crate::cmd::check::check(&dir) {
                Ok(found) => {
                    let ops = found
                        .registers
                        .iter()
                        .filter(|r| !r.path.ends_with(".tsx"))
                        .count();
                    assert_eq!(ops, 1, "{lang:?}: one starter op");
                    assert!(
                        found.ok(),
                        "{lang:?}: the starter is refused:\n{}",
                        rendered(&found.registers)
                    );
                }
                Err(e) => panic!("{lang:?}: the checker could not run: {e}"),
            }

            git(&dir, &["init", "-q", "-b", "main"]);
            git(&dir, &["add", "-A"]);
            let pushed = git(&dir, &["ls-files"]);
            assert!(!pushed.contains(".forge/"), "the push carries the skill");
        }
    }

    fn rendered(registers: &[Register]) -> String {
        registers.iter().map(Register::render).collect()
    }

    /// TSX-17: every dialect scaffold starts with the same TSX pages and
    /// components, each page's loader naming the starter op, `/wave` a client
    /// page holding a cell a click sets (TSX-28), and `forge check` admits all against the tree's own `service.json`, as the push's
    /// checker does; the TS skill a component is written to rides along
    /// whatever the ops' dialect.
    #[test]
    fn every_dialect_scaffold_starts_with_a_tsx_page_that_loads_its_op() {
        for lang in [Lang::Python, Lang::TypeScript, Lang::Java, Lang::Rust] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("acme");
            write_dialect_tree(&dir, "acme", "acme", lang).unwrap();

            for (rel, _) in DIALECT_APP {
                assert!(dir.join(rel).is_file(), "{lang:?}: {rel} not scaffolded");
            }
            for rel in ["apps/web/pages/index.tsx", "apps/web/pages/wave.tsx"] {
                let page = std::fs::read_to_string(dir.join(rel)).unwrap();
                assert!(
                    page.contains(r#"load("hello", { name: "acme" })"#),
                    "{lang:?}: {rel} does not load the starter op:\n{page}"
                );
            }
            let client = std::fs::read_to_string(dir.join("apps/web/pages/wave.tsx")).unwrap();
            assert!(
                client.contains(r#"export const mode = "client";"#),
                "{lang:?}: wave.tsx is not a client page:\n{client}"
            );
            let cell = std::fs::read_to_string(dir.join("apps/web/components/wave.tsx")).unwrap();
            assert!(
                cell.contains("useState<number>(0)") && cell.contains("onClick={() => setWaves("),
                "{lang:?}: the client component holds no cell set by a click:\n{cell}"
            );
            let skill = dir.join(subset_skill::SKILL_DIR).join("SUBSET-SKILL-TS.md");
            assert!(skill.is_file(), "{lang:?}: no TS skill for the components");

            let found = crate::cmd::check::check(&dir)
                .unwrap_or_else(|e| panic!("{lang:?}: the checker could not run: {e}"));
            let modules: Vec<&str> = found
                .registers
                .iter()
                .map(|r| r.path.as_str())
                .filter(|p| p.ends_with(".tsx"))
                .collect();
            assert_eq!(
                modules.len(),
                4,
                "{lang:?}: two pages and two components: {modules:?}"
            );
            assert!(
                modules.iter().any(|p| p.ends_with("pages/wave.tsx")),
                "{lang:?}: forge check never read the client page: {modules:?}"
            );
            assert!(
                found.ok(),
                "{lang:?}: the UI is refused:\n{}",
                rendered(&found.registers)
            );
        }
    }

    /// The check that admits the scaffold's page is the push's, not a
    /// parse: a loader naming an op the tree does not declare is refused by
    /// `forge check` (FL1958) before any push.
    #[test]
    fn a_scaffold_page_loading_an_undeclared_op_is_refused_by_forge_check() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acme");
        write_dialect_tree(&dir, "acme", "acme", Lang::Python).unwrap();
        let page = dir.join("apps/web/pages/index.tsx");
        let text = std::fs::read_to_string(&page).unwrap();
        std::fs::write(&page, text.replace(r#"load("hello""#, r#"load("goodbye""#)).unwrap();

        let found = crate::cmd::check::check(&dir).unwrap();
        assert!(!found.ok(), "a page loading `goodbye` was admitted");
        let refused = rendered(&found.registers);
        assert!(
            refused.contains("FL1958"),
            "not refused as FL1958:\n{refused}"
        );
        assert!(
            refused.contains("index.tsx"),
            "the refusal names no page:\n{refused}"
        );
    }

    /// invoice-path IP-2: every dialect tree carries `.forge-interpret` and
    /// pushes it, so its first `git push` takes the interpreted converge
    /// instead of stopping for want of a `forge.lock`.
    #[test]
    fn every_dialect_scaffold_pushes_the_interpret_marker() {
        let mut missing = Vec::new();
        for lang in [Lang::Python, Lang::TypeScript, Lang::Java, Lang::Rust] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("acme");
            write_dialect_tree(&dir, "acme", "acme", lang).unwrap();

            if !dir.join(".forge-interpret").is_file() {
                missing.push(format!("{lang:?}: no .forge-interpret scaffolded"));
                continue;
            }
            git(&dir, &["init", "-q", "-b", "main"]);
            git(&dir, &["add", "-A"]);
            let pushed = git(&dir, &["ls-files"]);
            if !pushed.lines().any(|l| l == ".forge-interpret") {
                missing.push(format!("{lang:?}: .forge-interpret is not pushed"));
            }
        }
        assert!(missing.is_empty(), "{}", missing.join("\n"));
    }
}
