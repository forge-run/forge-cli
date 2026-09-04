//! Where a workspace keeps its dialect sources, and which schemas check them.
//!
//! Two questions, one answer each, in one place because `forge check` and
//! `forge wasm-build` must give the same ones. A checker that reads a
//! different set of files from the builder is a checker whose green means
//! nothing.
//!
//! The server half asks the same two questions of a pushed tree rather than
//! of a directory
//! (`forge-control-plane/src/git/reconcile/dialect.rs`); the shapes are
//! deliberately identical.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use forge_lang_rustgen::{
    CompileError, CompileSpec, Compiled, Defect, EMITTER_VERSION, Layout, Plants, Tables,
    compile_all, lang_of,
};

/// The reference directory a workspace keeps runtime-owned table schemas in.
///
/// Read for CHECKING and never deployed: the workspace compiler reads only
/// `domains/*/schemas`, so a table declared here creates nothing on converge.
/// That is the point — 26 of the 27 tables the dogfood ops read belong to the
/// runtime rather than to the workspace, and rule zero needs their column
/// types to accept a `storage.query` against them.
pub const PLATFORM_SCHEMAS: &str = "platform-schemas";

/// Where a SERVER-side build materializes the emitted crate's two path
/// dependencies. Nothing is written there on this side; the generated root
/// manifest names it in `exclude` so one manifest satisfies both builders.
/// Kept in step with `forge-control-plane`'s `git::reconcile::dialect::DEPS_DIR`
/// — the two are one string in two repos, which is what the exclude check on
/// the server enforces.
pub const DEPS_DIR: &str = ".forge-deps";

/// Every dialect source in the workspace, sorted.
///
/// `domains/<d>/services/*` and nothing deeper: a source under a
/// subdirectory of `services/` is not an op module, and treating it as one
/// would check — and later emit — a crate the author did not ask for.
pub fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for domain in domains(root) {
        found.extend(sources_of_in(&domain, rust_is_dialect(root)));
    }
    found.sort();
    found
}

/// Whether `.rs` counts as a dialect source in THIS workspace.
///
/// P10.7 gave `lang_of` a Rust arm, and that collides head-on with the
/// raw-Rust layout: the portal's hand-written ops live at exactly
/// `domains/<d>/services/ops.rs`. The tie-break is the WORKSPACE'S ROUTE,
/// not the file: a tree that opted into the interpreted tier
/// (`.forge-interpret` at its root) reads `.rs` as the Rust DIALECT; an
/// unmarked workspace keeps the raw meaning and must not notice the
/// dialect exists (`a_rust_only_workspace_is_left_exactly_as_it_was`).
/// The server half applies the same rule over a pushed tree
/// (`forge-control-plane`'s `dialect::rust_is_dialect`).
pub fn rust_is_dialect(root: &Path) -> bool {
    root.join(".forge-interpret").is_file()
}

/// One domain's dialect sources, sorted — so the emitted crate is a function
/// of the tree and not of a directory read order.
///
/// WHICH extensions count is [`forge_lang_rustgen::lang_of`]'s answer and not
/// this file's. It used to be a `DIALECT_SUFFIX` constant here, which was a
/// second copy of a map the toolchain already owned: the day forge-lang grew
/// a second front half, a checker that had not been told would walk past
/// every source of the new language and report a workspace with nothing in
/// it. Discovery and dispatch have to agree, so they read one function.
pub fn sources_of(domain: &Path) -> Vec<PathBuf> {
    // The domain's parent's parent is the workspace root
    // (`<root>/domains/<d>`), which is where the Rust ruling lives.
    let rust_dialect = domain
        .parent()
        .and_then(|p| p.parent())
        .map(rust_is_dialect)
        .unwrap_or(false);
    sources_of_in(domain, rust_dialect)
}

fn sources_of_in(domain: &Path, rust_dialect: bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(domain.join("services")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && match lang_of(p) {
                    Some(forge_lang_rustgen::Lang::Rust) => rust_dialect,
                    Some(_) => true,
                    None => false,
                }
        })
        .collect();
    found.sort();
    found
}

/// The workspace's domain directories, sorted.
pub fn domains(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join("domains")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The directories rule zero loads table schemas from.
///
/// MEASURED, not chosen. With only its own domain's schemas plus the platform
/// set, 24 of the dogfood workspace's 44 sources are refused for reading a
/// table no loaded schema declares — billing reads `tenant_memberships`,
/// `tenants` and `workspaces`, tenancy reads `users`. With every domain's
/// schemas, all 44 are accepted. Domains read across each other, so the scope
/// is the WORKSPACE and not the domain.
pub fn schema_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = domains(root)
        .into_iter()
        .map(|d| d.join("schemas"))
        .filter(|d| d.is_dir())
        .collect();
    let platform = root.join(PLATFORM_SCHEMAS);
    if platform.is_dir() {
        roots.push(platform);
    }
    roots
}

/// The op names a domain's `service.json` DECLARES.
///
/// `None` when there is no `service.json` — a domain may legitimately have
/// none yet, and "no declaration file" is not "declares nothing". The two
/// authored shapes are both accepted, exactly as `contract_lint` accepts
/// them: the flat per-domain `{name, domain, operations:[…]}` and the full
/// `{services:[…]}` deploy manifest.
pub fn declared_ops(domain: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(domain.join("service.json")).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut names = Vec::new();
    let mut take = |ops: &serde_json::Value| {
        if let Some(list) = ops.as_array() {
            names.extend(
                list.iter()
                    .filter_map(|o| o.get("name")?.as_str().map(str::to_string)),
            );
        }
    };
    match doc.get("services").and_then(|v| v.as_array()) {
        Some(services) => {
            for svc in services {
                if let Some(ops) = svc.get("operations") {
                    take(ops);
                }
            }
        }
        None => {
            if let Some(ops) = doc.get("operations") {
                take(ops);
            }
        }
    }
    Some(names)
}

/// One disagreement between what a domain declares and what it defines.
#[derive(Debug)]
pub struct OpMismatch {
    pub domain: String,
    pub op: String,
    pub kind: MismatchKind,
}

#[derive(Debug)]
pub enum MismatchKind {
    /// In `service.json`, defined by no source. The route exists in the
    /// contract and 404s at runtime.
    DeclaredNotDefined,
    /// Defined by a source, in no `service.json`. The op has no contract, so
    /// it is unreachable through the API and invisible to the SDK generator —
    /// code that compiles, deploys, and can never be called.
    DefinedNotDeclared,
}

impl OpMismatch {
    /// The line an author reads.
    pub fn render(&self) -> String {
        match self.kind {
            MismatchKind::DeclaredNotDefined => format!(
                "{}: service.json declares `{}` and no source defines it — \
                 the route is in the contract and 404s at runtime",
                self.domain, self.op
            ),
            MismatchKind::DefinedNotDeclared => format!(
                "{}: `{}` is defined and service.json does not declare it — \
                 no contract, so it is unreachable through the API and absent \
                 from the generated SDK",
                self.domain, self.op
            ),
        }
    }
}

/// Compare what each dialect domain declares against what it defines.
///
/// Dialect domains only. A Rust domain's ops are not extractable without
/// parsing Rust, which the workspace compiler explicitly does not do — so
/// silence about them is honest, and claiming otherwise would make this check
/// wrong for the portal.
pub fn op_mismatches(root: &Path, defined: &BTreeMap<String, Vec<String>>) -> Vec<OpMismatch> {
    let mut out = Vec::new();
    for dir in domains(root) {
        let name = domain_name(&dir);
        let Some(defined_here) = defined.get(&name) else {
            continue; // not a dialect domain
        };
        let Some(declared) = declared_ops(&dir) else {
            continue; // no service.json authored yet
        };
        for op in &declared {
            if !defined_here.contains(op) {
                out.push(OpMismatch {
                    domain: name.clone(),
                    op: op.clone(),
                    kind: MismatchKind::DeclaredNotDefined,
                });
            }
        }
        for op in defined_here {
            if !declared.contains(op) {
                out.push(OpMismatch {
                    domain: name.clone(),
                    op: op.clone(),
                    kind: MismatchKind::DefinedNotDeclared,
                });
            }
        }
    }
    out
}

/// The cargo package an emitted domain crate is named, and the workspace
/// member path it lives at.
///
/// Both are conventions the build already hard-codes:
/// `stamp_built_wasm_hashes` looks for `forge_domain_<d>.wasm`
/// (`crate::cmd::build`), and the portal's own Rust domains are members at
/// `domains/<d>`. The dialect follows them rather than inventing a third.
pub fn crate_name(domain: &Path) -> String {
    format!("forge-domain-{}", domain_name(domain))
}

/// A domain directory's name.
pub fn domain_name(domain: &Path) -> String {
    domain
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One domain, transpiled to a crate on disk — or, in MIXED domains
/// (a hand-written crate hosting dialect sources; portal-java-cutover J-5
/// rung 2), to a `services/jgen` module inside that crate.
pub struct EmittedDomain {
    /// The domain's name — also the `<d>::<d>` lock key's halves.
    pub name: String,
    /// The crate's path relative to the workspace root, which is also its
    /// workspace-member path.
    pub member: String,
    /// The crate as emitted (crate mode), kept so a compile failure can be
    /// bundled without re-rendering. `None` in module mode — the member
    /// crate is the customer's own, and its compile errors are its own too
    /// except inside `services/jgen/`, which the generic build failure
    /// still names loudly.
    pub compiled: Option<Compiled>,
    /// The emitted module's op roster (module mode). `None` in crate mode.
    pub module: Option<forge_lang_rustgen::CompiledModule>,
}

/// Transpile every dialect domain in the workspace to a crate beside its
/// sources, and make sure the workspace manifest builds them.
///
/// Runs after the graph compiler knows the module graph and BEFORE the one
/// `cargo build` — the crate has to exist when cargo reads the workspace, and
/// it has to be a MEMBER of it, because feature unification (and therefore
/// byte-parity with the server) is a property of that one invocation rather
/// than of any crate in it (`forge-lang/…/tests/parity.rs`).
///
/// The crate lands at `domains/<d>/`, which is where the portal's Rust domain
/// crates already live: same member path, same `forge-domain-<d>` package
/// name, so the rest of the build does not learn a second convention.
pub fn emit(root: &Path) -> Result<Vec<EmittedDomain>> {
    let domains: Vec<(PathBuf, Vec<PathBuf>)> = domains(root)
        .into_iter()
        .map(|d| {
            let sources = sources_of(&d);
            (d, sources)
        })
        .filter(|(_, sources)| !sources.is_empty())
        .collect();
    if domains.is_empty() {
        return Ok(Vec::new());
    }

    let schema_dirs = schema_roots(root);
    let tables = match schema_dirs.is_empty() {
        true => None,
        false => Some(
            Tables::load(&schema_dirs)
                .map_err(|e| anyhow::anyhow!("cannot load the workspace's table schemas: {e}"))?,
        ),
    };

    let mut emitted = Vec::with_capacity(domains.len());
    for (dir, sources) in &domains {
        let name = domain_name(dir);
        // MIXED domain (J-5 rung 2): a hand-written crate (its own
        // `services/lib.rs`) hosting dialect sources. Emit a `services/jgen`
        // module for the host's fallback arm instead of replacing the crate;
        // the member list and the domain manifest stay the author's.
        if dir.join("services/lib.rs").is_file() {
            let module = forge_lang_rustgen::compile_module(
                sources,
                tables.as_ref(),
                &forge_lang_rustgen::ModuleSpec {
                    out_dir: dir.join("services/jgen"),
                    source_root: Some(root.to_path_buf()),
                },
            )
            .map_err(|e| refusal(&name, e))?;
            emitted.push(EmittedDomain {
                name,
                member: format!("domains/{}", domain_name(dir)),
                compiled: None,
                module: Some(module),
            });
            continue;
        }
        let compiled = compile_all(
            sources,
            tables.as_ref(),
            &CompileSpec {
                out_dir: dir.clone(),
                crate_name: Some(crate_name(dir)),
                // The two dependencies stay where they are — flat siblings of
                // the workspace, which is the layout the working root
                // guarantees. Nothing is copied into the workspace, so the
                // root manifest needs `members` and nothing else.
                //
                // Where a RELEASED forge-cli finds them is not answered here
                // and not answerable by copying either: the sources have to
                // come from somewhere on that machine. It is the open question
                // `forge-lang/.../build_root.rs` names, and its answer is git
                // dependencies pinned by revision (PHASE5-PROPOSAL §3.3).
                rt_path: None,
                sdk_path: None,
                trace: false,
                plants: Plants::none(),
                layout: Layout::WorkspaceMember,
                build_root: None,
                // Emit workspace-relative source labels so the committed
                // crate is reproducible across checkouts — a portal domain's
                // emitted crate is committed and CI checks it out at a
                // different path (J-5: the S2 cutover requires this).
                source_root: Some(root.to_path_buf()),
            },
        )
        .map_err(|e| refusal(&name, e))?;
        emitted.push(EmittedDomain {
            name,
            member: format!("domains/{}", domain_name(dir)),
            compiled: Some(compiled),
            module: None,
        });
    }

    workspace_manifest(root, &emitted)?;
    Ok(emitted)
}

/// The root `Cargo.toml`, generated when there is none and CHECKED when there
/// is one.
///
/// A dialect-only workspace has no Rust to have authored a manifest for, so
/// one is generated. A workspace that HAS one is a workspace whose author
/// maintains it, and adding a member behind their back would produce a build
/// they cannot reproduce by reading their own tree. So: a member it does not
/// list is a refusal naming the exact line to add.
fn workspace_manifest(root: &Path, emitted: &[EmittedDomain]) -> Result<()> {
    let path = root.join("Cargo.toml");
    if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let missing: Vec<&str> = emitted
            .iter()
            .filter(|e| e.compiled.is_some())
            .map(|e| e.member.as_str())
            .filter(|m| !text.contains(m))
            .collect();
        if !missing.is_empty() {
            bail!(
                "{} does not list the emitted dialect crate(s) as workspace \
                 members. Add to its [workspace] members: {}",
                path.display(),
                missing
                    .iter()
                    .map(|m| format!("\"{m}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        return Ok(());
    }
    std::fs::write(
        &path,
        format!(
            "# Generated by forge-lang {EMITTER_VERSION}. Do not edit — regenerate with \
             `forge wasm-build`.\n\
             #\n\
             # A dialect workspace has no hand-written Rust, so nothing else would\n\
             # have authored this. Every member is an emitted `forge-domain-<d>`; the\n\
             # build is ONE cargo invocation over all of them, because feature\n\
             # unification is a property of the invocation and byte-parity rests on it.\n\
             #\n\
             # COMMIT THIS FILE. The emitted crates under `domains/<d>/src` are\n\
             # generated and belong in .gitignore; this manifest is what tells a\n\
             # server-side build which members to expect, so it has to be in the tree.\n\
             [workspace]\n\
             resolver = \"2\"\n\
             members = [{}]\n\
             # `{DEPS_DIR}` is where a SERVER-side build materializes forge-lang-rt and\n\
             # forge-sdk-v2, which the pushed tree cannot carry. A path dependency under\n\
             # a workspace root has to be a member or excluded, and those two are\n\
             # dependencies rather than members. Nothing is copied there on this side —\n\
             # the line is here so ONE generated manifest satisfies both builders.\n\
             exclude = [\"{DEPS_DIR}\"]\n",
            emitted
                .iter()
                .map(|e| format!("\"{}\"", e.member))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;
    Ok(())
}

/// A compile failure as the author reads it.
fn refusal(domain: &str, e: CompileError) -> anyhow::Error {
    match e {
        CompileError::Rejected(register) => anyhow::anyhow!(
            "domain `{domain}` was refused by the dialect checker:\n{}\n\
             (`forge check` reports every source's verdict at once)",
            register.render().trim_end()
        ),
        CompileError::NoOps(text) | CompileError::Conflict(text) => {
            anyhow::anyhow!("domain `{domain}`: {}", text.trim_end())
        }
        CompileError::Toolchain(m) => anyhow::anyhow!(
            "domain `{domain}`: the dialect toolchain could not run: {m} \
             (the front end validates with CPython — is python3 on PATH?)"
        ),
    }
}

/// After a failed workspace build: is a dialect crate implicated, and if so,
/// bundle it.
///
/// Returns `None` when nothing points at emitted code — a Rust workspace's
/// build failure is the customer's Rust, and rustc's output is exactly what
/// they need. Handing them a "this is our bug" message for their own type
/// error would be worse than saying nothing.
///
/// The build that just failed streamed its output to the terminal, which is
/// right for a long build and useless for a bundle. So the diagnostics are
/// re-collected with `--message-format=json` — a second cargo run that costs
/// nothing, because everything is already compiled or already failed and
/// cargo replays the same errors from cache.
pub fn report_defect(root: &Path, emitted: &[EmittedDomain]) -> Result<Option<String>> {
    if emitted.is_empty() {
        return Ok(None);
    }
    let out = std::process::Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--message-format=json",
        ])
        .current_dir(root)
        .output()?;
    let diagnostics = String::from_utf8_lossy(&out.stdout);
    if !forge_lang_rustgen::is_compile_defect(&diagnostics) {
        // Cargo failed before compiling anything — an unresolvable dependency,
        // a missing target. That is the build environment, not the emitter,
        // and blaming ourselves for it teaches people to ignore the blame.
        return Ok(None);
    }

    // Which emitted crates does cargo actually name? A workspace can hold Rust
    // domains too, and their errors are theirs.
    let mut reports = Vec::new();
    for domain in emitted {
        let Some(compiled) = &domain.compiled else {
            // Module mode: the member crate is hand-written; only its
            // `services/jgen` is ours, and the generic failure names it.
            continue;
        };
        if !diagnostics.contains(&format!("{}/src/", domain.member)) {
            continue;
        }
        let sources = sources_of(&root.join(&domain.member));
        let path = Defect {
            sources: &sources,
            emitted: &compiled.emitted,
            crate_name: &compiled.crate_name,
            diagnostics: &diagnostics,
        }
        .write_report(&root.join(&domain.member))?;
        reports.push((domain.name.clone(), path));
    }
    if reports.is_empty() {
        return Ok(None);
    }

    let mut msg = String::from(
        "the emitted crate(s) below do not compile.\n\n\
         That is a forge-lang defect, not an error in your source — emitted Rust\n\
         is compile-clean by construction for anything `forge check` accepts.\n\
         There is nothing to change in your `.py`, and editing the generated\n\
         crate will not help: the next build overwrites it.\n\n\
         Report bundle(s) — each holds the source, the emitted crate, cargo's\n\
         diagnostics verbatim, and the toolchain versions:\n",
    );
    for (domain, path) in &reports {
        msg.push_str(&format!("  {domain}: {}\n", path.display()));
    }
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for path in paths {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, b"").unwrap();
        }
        dir
    }

    #[test]
    fn sources_are_the_services_directory_and_nothing_deeper() {
        let ws = workspace(&[
            "domains/billing/services/plan_catalog.py",
            "domains/billing/services/plan_detail.py",
            "domains/billing/services/ops.rs",
            "domains/billing/services/nested/helper.py",
            "domains/billing/notes.py",
            "scripts/tool.py",
        ]);
        let found: Vec<String> = sources(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/billing/services/plan_catalog.py",
                "domains/billing/services/plan_detail.py",
            ]
        );
    }

    /// The measurement in `schema_roots`' doc, as an assertion: every domain's
    /// schemas, plus the platform reference set, because domains read across
    /// each other.
    #[test]
    fn the_schema_scope_is_the_whole_workspace() {
        let ws = workspace(&[
            "domains/billing/schemas/plan_products.table.json",
            "domains/tenancy/schemas/tenants.table.json",
            // A domain with no schemas of its own still reads the others'.
            "domains/release/services/workspace_overview.py",
            "platform-schemas/_tokens.table.json",
        ]);
        let found: Vec<String> = schema_roots(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/billing/schemas",
                "domains/tenancy/schemas",
                "platform-schemas",
            ]
        );
    }

    const ACCEPTED: &str = r#"from dataclasses import dataclass

from forge import OpContext, Value, op


@dataclass
class Greeting:
    text: str


@op("hello")
def hello(ctx: OpContext, input: Value) -> Greeting:
    return Greeting(text="hi")
"#;

    /// The same op in the OTHER dialect spelling.
    ///
    /// Deliberately the TS twin of `ACCEPTED` rather than a wider fixture:
    /// what these tests hold is that discovery and emission read a `.ts`
    /// source at all, and a fixture that also exercised the checker would
    /// fail for two reasons and be diagnosed for one.
    const ACCEPTED_TS: &str = r#"import { op, OpContext, Value } from "forge";

interface Greeting {
  text: string;
}

export const hello = op("hello", (ctx: OpContext, input: Value) => {
  const g: Greeting = { text: "hi" };
  return g;
});
"#;

    /// And the THIRD spelling (P6.6).
    ///
    /// Same narrowness as the TS twin above and for the same reason. The
    /// Java shape differs from both neighbours where the language forces it:
    /// the public type's name is the file's, the op keeps its snake_case
    /// name rather than taking the class's casing, and the output shape is a
    /// `record` because the dialect has no dataclass and no interface here.
    const ACCEPTED_JAVA: &str = r#"import forge.Op;
import forge.OpContext;
import forge.Value;

public final class Hello {
    public record Greeting(String text) {}

    @Op("hello")
    public static Greeting hello(OpContext ctx, Value input) {
        return new Greeting("hi");
    }
}
"#;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        dir
    }

    fn skip_without_cpython(e: &str) -> bool {
        let missing = e.contains("python3") || e.contains("CPython");
        if missing {
            eprintln!("skipping: {e}");
        }
        missing
    }

    /// The emitted crate lands where the portal's Rust domains already are, is
    /// a MEMBER rather than its own workspace root, and the generated root
    /// manifest carries both things a build needs: the member list, and the
    /// exclude the SERVER's materialized dependencies require. One manifest,
    /// both builders.
    #[test]
    fn emitting_a_domain_writes_a_member_crate_and_a_root_manifest() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("domains/greetings/services/hello.py", ACCEPTED),
        ]);
        let emitted = match emit(ws.path()) {
            Ok(e) => e,
            Err(e) if skip_without_cpython(&format!("{e:#}")) => return,
            Err(e) => panic!("{e:#}"),
        };
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].name, "greetings");
        assert_eq!(emitted[0].member, "domains/greetings");

        let member =
            std::fs::read_to_string(ws.path().join("domains/greetings/Cargo.toml")).unwrap();
        assert!(
            member.contains("name = \"forge-domain-greetings\""),
            "{member}"
        );
        assert!(
            !member.contains("[workspace]"),
            "an emitted domain must be a workspace MEMBER:\n{member}"
        );
        assert!(ws.path().join("domains/greetings/src/lib.rs").is_file());
        // The dialect source is still the source of truth, beside its output.
        assert!(
            ws.path()
                .join("domains/greetings/services/hello.py")
                .is_file()
        );

        let root = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
        assert!(root.contains("members = [\"domains/greetings\"]"), "{root}");
        assert!(
            root.contains(&format!("exclude = [\"{DEPS_DIR}\"]")),
            "{root}"
        );
    }

    /// The TS twin of the test above, asserting the same things.
    ///
    /// One builder, two spellings: a `.ts` source has to be discovered,
    /// checked and emitted through exactly the path a `.py` source takes,
    /// because the crate that comes out the other end is the same crate.
    /// Before P7.5-builders discovery was gated on `.py` alone, so this tree
    /// emitted nothing — and `forge wasm-build` then went looking for
    /// `forge-domain-greetings`'s wasm (forge-web already counts a `.ts`
    /// service as a wasm module) and died reading an artifact that was never
    /// produced, naming a path under `target/` rather than the customer's
    /// file.
    ///
    /// No `skip_without_cpython` here, and that is not an omission: the TS
    /// front half runs no reference toolchain, so this arm holds on a machine
    /// with no python3 at all.
    #[test]
    fn emitting_a_ts_domain_writes_a_member_crate_and_a_root_manifest() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("domains/greetings/services/hello.ts", ACCEPTED_TS),
        ]);
        let emitted = emit(ws.path()).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].name, "greetings");
        assert_eq!(emitted[0].member, "domains/greetings");

        let member =
            std::fs::read_to_string(ws.path().join("domains/greetings/Cargo.toml")).unwrap();
        assert!(
            member.contains("name = \"forge-domain-greetings\""),
            "{member}"
        );
        assert!(
            !member.contains("[workspace]"),
            "an emitted domain must be a workspace MEMBER:\n{member}"
        );
        assert!(ws.path().join("domains/greetings/src/lib.rs").is_file());
        // The dialect source is still the source of truth, beside its output.
        assert!(
            ws.path()
                .join("domains/greetings/services/hello.ts")
                .is_file()
        );

        let root = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
        assert!(root.contains("members = [\"domains/greetings\"]"), "{root}");
        assert!(
            root.contains(&format!("exclude = [\"{DEPS_DIR}\"]")),
            "{root}"
        );
    }

    /// The Java twin, asserting the same things (P6.6).
    ///
    /// One builder, three spellings now. The crate that comes out is the same
    /// crate whichever front half read the source, and this is where that is
    /// either true or visibly false — the emitted member is
    /// `forge-domain-greetings` from a `.java` source exactly as from a `.py`
    /// one, and the Java file stays beside its output as the source of truth.
    ///
    /// Unlike the TS twin this one DOES need a toolchain: javac is the Java
    /// front half's parser, so a machine with no JDK cannot read the source.
    /// That answers a named toolchain fault rather than a rejection, and this
    /// test fails loudly on such a machine rather than skipping — "could not
    /// run" is not "ran and was fine".
    #[test]
    fn emitting_a_java_domain_writes_a_member_crate_and_a_root_manifest() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("domains/greetings/services/Hello.java", ACCEPTED_JAVA),
        ]);
        let emitted = emit(ws.path()).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].name, "greetings");
        assert_eq!(emitted[0].member, "domains/greetings");

        let member =
            std::fs::read_to_string(ws.path().join("domains/greetings/Cargo.toml")).unwrap();
        assert!(
            member.contains("name = \"forge-domain-greetings\""),
            "{member}"
        );
        assert!(
            !member.contains("[workspace]"),
            "an emitted domain must be a workspace MEMBER:\n{member}"
        );
        assert!(ws.path().join("domains/greetings/src/lib.rs").is_file());
        assert!(
            ws.path()
                .join("domains/greetings/services/Hello.java")
                .is_file()
        );

        let root = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
        assert!(root.contains("members = [\"domains/greetings\"]"), "{root}");
        assert!(
            root.contains(&format!("exclude = [\"{DEPS_DIR}\"]")),
            "{root}"
        );
    }

    /// Discovery finds all THREE spellings under one domain, and sorts them.
    ///
    /// The multi-source case is where a domain's crate is emitted from
    /// several modules, so a discovery that found only some of them would
    /// emit a crate missing part of its ops rather than failing. The three
    /// non-sources beside them are the control: discovery is `lang_of`, not
    /// "every file under services/".
    #[test]
    fn discovery_finds_all_three_spellings_and_nothing_else() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("domains/greetings/services/hello.ts", ACCEPTED_TS),
            ("domains/greetings/services/aloha.py", ACCEPTED),
            // A stub rather than ACCEPTED_JAVA on purpose: discovery is
            // `lang_of` over an extension and parses nothing, and a public
            // Java class whose name did not match this file would be a javac
            // error the moment anything DID parse it.
            ("domains/greetings/services/Namaste.java", "// java"),
            ("domains/greetings/services/README.md", "notes"),
            ("domains/greetings/services/ops.rs", "// rust"),
            ("domains/greetings/services/nested/deep.py", ACCEPTED),
        ]);
        let found: Vec<String> = sources(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/greetings/services/Namaste.java",
                "domains/greetings/services/aloha.py",
                "domains/greetings/services/hello.ts",
            ]
        );
    }

    /// The load-bearing negative: a Rust workspace must not notice that the
    /// dialect exists. No crate, no generated manifest, no lock field — the
    /// same bar P5.4's control test set for the workspace compiler, which
    /// proved a Rust-only tree gets no phantom redeploy.
    /// The positive twin of the two pins around it: a workspace that opted
    /// into the interpreted tier reads `.rs` as the Rust DIALECT (P10.7's
    /// tie-break — the route decides, not the extension).
    #[test]
    fn a_marked_workspace_reads_rs_as_the_rust_dialect() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            (".forge-interpret", "1"),
            ("domains/greetings/services/aloha.py", ACCEPTED),
            ("domains/greetings/services/ops.rs", "// dialect rust"),
        ]);
        let found: Vec<String> = sources(ws.path())
            .iter()
            .map(|p| p.strip_prefix(ws.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "domains/greetings/services/aloha.py",
                "domains/greetings/services/ops.rs",
            ]
        );
    }

    #[test]
    fn a_rust_only_workspace_is_left_exactly_as_it_was() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("domains/billing/service.json", "{}"),
            ("domains/billing/services/ops.rs", "// rust"),
        ]);
        assert!(emit(ws.path()).unwrap().is_empty());
        assert!(
            !ws.path().join("Cargo.toml").exists(),
            "a Rust workspace must not gain a generated manifest"
        );
    }

    /// A manifest the author wrote is never edited behind their back — the
    /// build they get has to be one they can reproduce by reading their tree.
    #[test]
    fn a_hand_written_root_manifest_missing_a_member_is_refused_with_the_line_to_add() {
        let ws = tree(&[
            ("workspace.json", "{}"),
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/shared\"]\n"),
            ("domains/greetings/services/hello.py", ACCEPTED),
        ]);
        let err = match emit(ws.path()) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("an unlisted member must be refused"),
        };
        if skip_without_cpython(&err) {
            return;
        }
        assert!(err.contains("does not list"), "{err}");
        assert!(err.contains("\"domains/greetings\""), "{err}");
        // And it was not rewritten.
        let root = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
        assert_eq!(root, "[workspace]\nmembers = [\"crates/shared\"]\n");
    }

    /// A Rust-only workspace must never see the dialect's "this is our bug"
    /// message.
    ///
    /// Its build failure is the customer's Rust, and rustc's output is exactly
    /// what they need. Telling them a forge-lang defect caused their own type
    /// error would be worse than saying nothing — and this returns before
    /// spawning cargo at all, so it costs a Rust workspace nothing.
    #[test]
    fn a_workspace_with_no_emitted_crate_is_never_blamed_on_the_emitter() {
        let ws = tree(&[("workspace.json", "{}")]);
        assert!(report_defect(ws.path(), &[]).unwrap().is_none());
    }

    #[test]
    fn a_domain_crate_is_named_the_way_the_build_already_looks_for_it() {
        assert_eq!(
            crate_name(Path::new("/w/domains/billing")),
            "forge-domain-billing"
        );
    }
}
