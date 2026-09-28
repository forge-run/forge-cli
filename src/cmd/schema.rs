//! `forge schema` — the compiled-schema snapshot (typed-schema TS-0).
//!
//! # What the snapshot is
//!
//! `forge schema compile` writes `schema.lock` beside `forge.lock`: ONE
//! deterministic, content-addressed projection of every table the converge
//! will apply for this workspace —
//!
//! - the authored `domains/*/schemas/*.table.json`, parsed through the same
//!   `SchemaDefinition::from_json` the storage apply step runs
//!   (`forge-storage/src/api/schema.rs`), so the snapshot cannot disagree
//!   with the apply about what a file means;
//! - the runtime-owned platform bundle
//!   (`forge-runtime/crates/forge-runtime-auth/schemas/`, applied at
//!   workspace boot by `bootstrap_owned_schemas`), embedded at CLI build
//!   time from the canonical path (`build.rs`) — pinned by ref, never
//!   copied into the tree;
//! - the system columns each table's archetype materializes (`id`,
//!   `created_at`, …), enumerated from the same generated `Archetype`
//!   catalog (`forge-types/types.yaml`) the substrate's auto-populate
//!   strategies fire from — never a transcribed list.
//!
//! Relationships are resolved against the whole set at once, so the
//! apply-order `relationship_unresolved_target` noise cannot exist here: a
//! target either resolves or the compile refuses. Each carries its KIND —
//! `has_many` or `belongs_to` — because the two arrive in different runtime
//! shapes and a consumer typing a followed read has to know which
//! (typed-schema TS-7).
//!
//! # What the snapshot is NOT
//!
//! An authority. The snapshot changes no schema semantics, performs no
//! migration, and adds no column: it is a PROJECTION of what the converge
//! already derives, for consumers that need the resolved view (the checker
//! first — `forge check` judges reads against it; later, generated types).
//! Canonical truth is build-locally-deterministic, the forge.lock
//! philosophy applied to schemas; the live registry only CONFIRMS
//! (advisory, the staleness-detector family — never a gate).
//!
//! # Drift
//!
//! The emitted-committed pattern: the snapshot records the sha256 of every
//! input, and both `forge schema compile --check` and `forge check` refuse
//! a snapshot whose inputs have moved in the same tree. A stale
//! `schema.lock` is a toolchain error naming the fix, never a silently
//! wrong verdict.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::dialect;
use crate::dialect::{PLATFORM_BUNDLE, PLATFORM_BUNDLE_SOURCE, bundle_hash, sha256};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use forge_lang_rustgen::{SNAPSHOT_FILE, Snapshot};
use forge_types::ForgeType;
use forge_types::schema::SchemaDefinition;
use forge_types::schema::metadata::{EnumDef, EnumRefOrInline, EnumValue};

#[derive(Debug, Subcommand)]
pub enum SchemaCmd {
    /// Compile the workspace's schema snapshot (`schema.lock`): authored
    /// table schemas + the runtime-owned bundle + archetype system columns,
    /// resolved, content-addressed, committed beside `forge.lock`.
    Compile(CompileArgs),

    /// Compare the committed snapshot against the LIVE workspace registry's
    /// applied schema (`/api/v1/manage/schema/introspect`). ADVISORY — the
    /// staleness-detector family: build-locally-deterministic is canonical
    /// and the live stack confirms, so this always exits 0 and never gates.
    Diff(DiffArgs),

    /// Author an op's contract: `forge schema op new <domain>::<op>` appends
    /// its `service.json` entry and writes a handler stub that binds the
    /// generated request and returns the generated response.
    #[command(subcommand)]
    Op(OpCmd),
}

#[derive(Debug, Subcommand)]
pub enum OpCmd {
    /// Declare a new op in `domains/<domain>/service.json` (empty input and
    /// output shapes, no errors), write its typed handler stub under
    /// `domains/<domain>/services/`, and refresh `.forge/types`.
    New(OpNewArgs),
}

#[derive(Debug, Args)]
pub struct OpNewArgs {
    /// The op to declare, as `<domain>::<op>`.
    target: String,

    /// The stub's language — `py`, `ts`, `java` or `rust`. Defaults to the
    /// language of the domain's existing ops; required when it has none.
    #[arg(long)]
    lang: Option<String>,

    /// The op's kind, `query` or `mutation`.
    #[arg(long, default_value = "mutation")]
    kind: String,

    /// Workspace root (the directory holding `workspace.json`). Defaults to
    /// the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct CompileArgs {
    /// Workspace root (the directory holding `workspace.json`). Defaults to
    /// the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,

    /// Verify only: recompile in memory and fail (exit 1) if the committed
    /// `schema.lock` does not match — the drift gate. Writes nothing.
    #[arg(long)]
    check: bool,
}

pub async fn run(
    cmd: SchemaCmd,
    client: impl FnOnce() -> Result<crate::client::ForgeClient>,
) -> Result<()> {
    match cmd {
        SchemaCmd::Compile(args) => compile(args),
        SchemaCmd::Diff(args) => diff(args, client()?).await,
        SchemaCmd::Op(OpCmd::New(args)) => {
            let root = args
                .manifest_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let written = op_new(&root, &args)?;
            for path in &written {
                eprintln!("wrote {}", path.display());
            }
            refresh_types(&root);
            Ok(())
        }
    }
}

fn compile(args: CompileArgs) -> Result<()> {
    let root = args.manifest_dir.unwrap_or_else(|| PathBuf::from("."));
    if !root.join("workspace.json").exists() {
        bail!(
            "no workspace.json at {} — `forge schema compile` reads a \
             workspace; pass --manifest-dir or run from the workspace root",
            root.display()
        );
    }
    let compiled = compile_snapshot(&root)?;
    let lock_path = root.join(SNAPSHOT_FILE);
    if args.check {
        let existing = std::fs::read_to_string(&lock_path).with_context(|| {
            format!(
                "no {} at {} — run `forge schema compile` first",
                SNAPSHOT_FILE,
                root.display()
            )
        })?;
        if existing != compiled.text {
            bail!(
                "{} is stale: the tree's schema inputs no longer match it \
                 (recompiled {}, committed {}) — run `forge schema compile` \
                 and commit the result",
                SNAPSHOT_FILE,
                compiled.content_hash,
                Snapshot::parse(&existing, SNAPSHOT_FILE)
                    .map(|s| s.content_hash)
                    .unwrap_or_else(|_| "unparseable".into()),
            );
        }
        eprintln!(
            "{}: up to date ({}, {} tables)",
            SNAPSHOT_FILE, compiled.content_hash, compiled.tables
        );
        return Ok(());
    }
    std::fs::write(&lock_path, &compiled.text)
        .with_context(|| format!("write {}", lock_path.display()))?;
    eprintln!(
        "wrote {} ({}, {} tables: {} authored + {} runtime-owned)",
        lock_path.display(),
        compiled.content_hash,
        compiled.tables,
        compiled.authored,
        compiled.bundle,
    );
    refresh_types(&root);
    Ok(())
}

/// Regenerate `.forge/types/` from the workspace's `schema.lock`: the
/// generated schema surface and the dialect library, in all four dialects,
/// for the engineer's editor to resolve (typed-boundary TB-6).
///
/// Never fails the command it rides on. The tree is for READING — the push,
/// `forge check` and the serving path never touch it — so a machine with no
/// JDK, or a schema the generator will not map, is reported and the command
/// goes on.
pub(crate) fn refresh_types(root: &Path) {
    refresh_surface(root);
    write_service_schema(root);
}

/// `.forge/types/service.schema.json`: the JSON Schema of `service.json`
/// (ST-6), for the editor the `forge new` template's `.vscode/settings.json`
/// points at it. Written after the surface, which replaces the directory.
fn write_service_schema(root: &Path) {
    let dir = root.join(forge_lang_rustgen::workspace_types::TYPES_DIR);
    let path = dir.join(SERVICE_SCHEMA_FILE);
    let text = serde_json::to_string_pretty(&forge_lang_rustgen::service_json_schema())
        .expect("the schema is plain data");
    let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, text + "\n"));
    if let Err(e) = written {
        eprintln!("⚠  {} not written: {e}", path.display());
    }
}

/// The editor's JSON Schema of `service.json`, inside `.forge/types/`.
pub(crate) const SERVICE_SCHEMA_FILE: &str = "service.schema.json";

/// The generated schema surface and dialect library, from `schema.lock`.
fn refresh_surface(root: &Path) {
    use forge_lang_rustgen::workspace_types;
    if !root.join(SNAPSHOT_FILE).is_file() {
        eprintln!(
            "types: no {SNAPSHOT_FILE} at {} — `forge schema compile` writes it, \
             and {} with it",
            root.display(),
            workspace_types::TYPES_DIR
        );
        return;
    }
    match workspace_types::write(root) {
        Ok(written) => {
            eprintln!(
                "wrote {} ({} files) — the schema your editor resolves; git-ignored, never pushed",
                written.root.display(),
                written.files
            );
            if let Err(why) = &written.java {
                let first = why.lines().next().unwrap_or_default();
                eprintln!("  java: no forge-schema.jar — {first}");
            }
        }
        Err(e) => eprintln!("⚠  {} not written: {e}", workspace_types::TYPES_DIR),
    }
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// Workspace root (the directory holding `schema.lock`). Defaults to
    /// the current directory.
    #[arg(long)]
    manifest_dir: Option<PathBuf>,

    /// Compare against the live workspace (the only mode; spelled out so a
    /// future `--against <file>` has somewhere to sit).
    #[arg(long)]
    live: bool,
}

/// The advisory live diff. Reads the committed snapshot, fetches the live
/// registry's introspection, and reports — always exit 0.
async fn diff(args: DiffArgs, client: crate::client::ForgeClient) -> Result<()> {
    if !args.live {
        bail!("`forge schema diff` compares against the live workspace: pass --live");
    }
    let root = args.manifest_dir.unwrap_or_else(|| PathBuf::from("."));
    let lock_path = root.join(SNAPSHOT_FILE);
    let text = std::fs::read_to_string(&lock_path).with_context(|| {
        format!(
            "no {} at {} — run `forge schema compile` first",
            SNAPSHOT_FILE,
            root.display()
        )
    })?;
    let doc: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", lock_path.display()))?;
    let snap_hash = doc["content_hash"].as_str().unwrap_or("?").to_string();

    // Local freshness first — a stale snapshot makes the live comparison
    // about the wrong bytes. Reported, not fatal: this command never gates.
    if let Ok(snap) = Snapshot::parse(&text, SNAPSHOT_FILE)
        && let Err(stale) = crate::dialect::verify_snapshot(&root, &snap)
    {
        eprintln!("note: {stale}\n");
    }

    let live: serde_json::Value = client
        .get_json("/api/v1/manage/schema/introspect")
        .await
        .map_err(|e| anyhow::anyhow!("introspect the live workspace: {e}"))?;
    let empty = Vec::new();
    let live_tables: BTreeMap<&str, &serde_json::Value> = live["tables"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|t| t["name"].as_str().map(|n| (n, t)))
        .collect();

    let mut findings: Vec<String> = Vec::new();
    let snap_tables = doc["tables"].as_object().cloned().unwrap_or_default();
    for (name, entry) in &snap_tables {
        let Some(live_t) = live_tables.get(name.as_str()) else {
            findings.push(format!("{name}: in the snapshot, not applied live"));
            continue;
        };
        let snap_cols: BTreeMap<&str, &str> = entry["columns"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|c| Some((c["name"].as_str()?, c["type"].as_str()?)))
            .collect();
        let live_cols: BTreeMap<&str, &str> = live_t["columns"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|c| Some((c["name"].as_str()?, c["type"].as_str()?)))
            .collect();
        for (col, ty) in &snap_cols {
            match live_cols.get(col) {
                None => findings.push(format!("{name}.{col}: in the snapshot, not live")),
                Some(live_ty) if live_ty != ty => findings.push(format!(
                    "{name}.{col}: snapshot type {ty}, live type {live_ty}"
                )),
                Some(_) => {}
            }
        }
        for col in live_cols.keys() {
            if !snap_cols.contains_key(col) {
                findings.push(format!(
                    "{name}.{col}: live column the snapshot does not carry"
                ));
            }
        }
    }
    // Live-only tables are INFORMATION, not drift: the substrate's
    // `_`-platform tables and anything another surface applied.
    let live_only: Vec<&str> = live_tables
        .keys()
        .filter(|n| !snap_tables.contains_key(**n))
        .copied()
        .collect();

    println!(
        "schema diff --live · snapshot {snap_hash} · {} snapshot tables · {} live tables",
        snap_tables.len(),
        live_tables.len()
    );
    match findings.is_empty() {
        true => println!("clean: every snapshot table is applied live with matching columns"),
        false => {
            println!("{} difference(s) — advisory, not a gate:", findings.len());
            for f in &findings {
                println!("  {f}");
            }
        }
    }
    if !live_only.is_empty() {
        println!(
            "live-only tables (substrate/platform or applied elsewhere): {}",
            live_only.join(", ")
        );
    }
    Ok(())
}

/// A compiled snapshot: the exact bytes `schema.lock` holds, plus the
/// numbers the human-facing summary prints.
#[derive(Debug)]
pub struct CompiledSnapshot {
    pub text: String,
    pub content_hash: String,
    pub tables: usize,
    pub authored: usize,
    pub bundle: usize,
}

/// Compile the snapshot for `root`. Deterministic: same tree + same CLI
/// build → byte-identical output (BTreeMap ordering everywhere, no
/// timestamps, no absolute paths).
pub fn compile_snapshot(root: &Path) -> Result<CompiledSnapshot> {
    // ── inputs ──────────────────────────────────────────────────────────
    // Authored: every domain's `schemas/*.table.json`, tree-relative paths
    // with forward slashes. `platform-schemas/` is deliberately NOT an
    // input: it was the checker-only reference copy this artifact
    // supersedes, and the converge never deploys it.
    let mut authored: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (rel, path) in dialect::authored_schema_files(root) {
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        authored.insert(rel, bytes);
    }

    // ── parse — the SAME parse the storage apply runs ───────────────────
    struct Entry {
        origin: String,
        schema: SchemaDefinition,
    }
    let mut tables: BTreeMap<String, Entry> = BTreeMap::new();
    let place = |origin: String, bytes: &[u8], tables: &mut BTreeMap<String, Entry>| {
        // Mirror the converge exactly: `accept_destructive` is a deploy
        // DIRECTIVE the apply step reads and strips before parsing
        // (`forge-runtime/.../deploy/manifest.rs`), not part of the schema.
        let mut json: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| anyhow::anyhow!("{origin}: not valid JSON: {e}"))?;
        if let Some(obj) = json.as_object_mut() {
            obj.remove("accept_destructive");
        }
        let bytes = serde_json::to_vec(&json).expect("re-serialize schema JSON");
        let schema =
            SchemaDefinition::from_json(&bytes).map_err(|e| anyhow::anyhow!("{origin}: {e}"))?;
        let name = schema.name().to_string();
        if let Some(prior) = tables.get(&name) {
            bail!(
                "the table `{name}` is declared twice: {} and {origin} — \
                 one workspace, one declaration per table",
                prior.origin
            );
        }
        tables.insert(name, Entry { origin, schema });
        Ok(())
    };
    for (rel, bytes) in &authored {
        place(rel.clone(), bytes, &mut tables)?;
    }
    for (file, text) in PLATFORM_BUNDLE {
        place(
            format!("platform-bundle:{file}"),
            text.as_bytes(),
            &mut tables,
        )?;
    }

    // ── resolve relationships against the whole set at once ────────────
    let names: Vec<String> = tables.keys().cloned().collect();
    let mut unresolved: Vec<String> = Vec::new();
    for entry in tables.values() {
        // Authored inputs only. The bundle's own relationships point at the
        // substrate-guaranteed tables it deliberately does not own (`users`,
        // `tenants`, `workspaces` — operator-extensible, created by their
        // existing helpers), so a workspace that does not author them is not
        // wrong. Authored relationships are this workspace's to get right,
        // and a typo fails HERE with the file named — instead of as the
        // apply-order `relationship_unresolved_target` audit noise.
        if entry.origin.starts_with("platform-bundle:") {
            continue;
        }
        for rel in entry.schema.relationships() {
            if let Some(target) = rel.target_table.as_deref()
                && !tables.contains_key(target)
            {
                unresolved.push(format!(
                    "{}: relationship `{}` targets `{target}`, which no \
                     input declares",
                    entry.origin, rel.name
                ));
            }
        }
    }
    if !unresolved.is_empty() {
        bail!(
            "unresolved relationship targets:\n  {}\n(known tables: {})",
            unresolved.join("\n  "),
            names.join(", ")
        );
    }

    // ── project ─────────────────────────────────────────────────────────
    let mut table_docs = serde_json::Map::new();
    for (name, entry) in &tables {
        let archetype = entry.schema.archetype();
        // The archetype's own contribution, from the generated catalog —
        // what marks a column as system-materialized.
        let system: Vec<String> = archetype
            .columns()
            .iter()
            .map(|c| c.name.to_string())
            .collect();
        let mut cols = Vec::new();
        for col in entry.schema.columns() {
            let mut doc = serde_json::Map::new();
            doc.insert("name".into(), col.name.clone().into());
            doc.insert("type".into(), format!("{:?}", col.forge_type).into());
            doc.insert("wire".into(), wire_of(col.forge_type).into());
            doc.insert("nullable".into(), col.constraint.nullable.into());
            if let Some(auto) = col.auto_strategy {
                doc.insert("auto".into(), format!("{auto:?}").into());
            }
            if col.primary_key {
                doc.insert("primary_key".into(), true.into());
            }
            // Presence only, never the value: the lock is a shape artifact,
            // and a checker refusing an insert that omits a NOT NULL column
            // needs to know only that the database will fill it.
            if col.default.is_some() {
                doc.insert("has_default".into(), true.into());
            }
            // The column's closed sets, the two storage refuses a write
            // outside (`forge-storage/src/mutation/validate.rs`). A named
            // enum that does not resolve writes nothing: storage imposes no
            // constraint on it, and a checker must never refuse what storage
            // admits. Picklist display text is not written — values only.
            if let Some(domain) = enum_doc(col.enum_ref.as_ref(), entry.schema.enums()) {
                doc.insert("enum".into(), domain);
            }
            if let Some(values) = &col.values {
                let values: Vec<serde_json::Value> =
                    values.iter().map(|v| v.value.clone().into()).collect();
                doc.insert("picklist".into(), values.into());
            }
            if system.contains(&col.name) {
                doc.insert("system".into(), true.into());
            }
            if let Some(fk) = &col.references {
                doc.insert(
                    "references".into(),
                    serde_json::json!({"table": fk.table, "column": fk.column}),
                );
            }
            cols.push(serde_json::Value::Object(doc));
        }
        let mut rels = Vec::new();
        for rel in entry.schema.relationships() {
            let Some(target) = rel.target_table.as_deref() else {
                continue;
            };
            // The KIND, resolved here rather than re-derived downstream
            // (typed-schema TS-7). The two kinds arrive in different runtime
            // shapes — a has-many is a nested array under the relationship's
            // own key, a belongs-to is FLATTENED onto the parent row with
            // dotted keys — so a consumer that types a followed read has to
            // know which it is. `is_has_many` is the same answer the storage
            // side gives; reading it here keeps the snapshot the one place
            // anyone asks, and keeps the authored `.table.json` out of every
            // downstream's hands.
            let mut doc = serde_json::Map::new();
            doc.insert("name".into(), rel.name.clone().into());
            doc.insert("target_table".into(), target.into());
            doc.insert(
                "kind".into(),
                if rel.is_has_many() {
                    "has_many"
                } else {
                    "belongs_to"
                }
                .into(),
            );
            // The keys the kind was read off, for the record: a snapshot that
            // says `has_many` without saying what it joins on is a claim the
            // reader cannot check.
            if let Some(local) = rel.local_key.as_deref() {
                doc.insert("local_key".into(), local.into());
            }
            if let Some(fk) = rel.foreign_key.as_deref() {
                doc.insert("foreign_key".into(), fk.into());
            }
            if let Some(parent) = rel.parent_key.as_deref() {
                doc.insert("parent_key".into(), parent.into());
            }
            rels.push(serde_json::Value::Object(doc));
        }
        let mut doc = serde_json::Map::new();
        doc.insert("origin".into(), entry.origin.clone().into());
        doc.insert("archetype".into(), format!("{archetype:?}").into());
        doc.insert("columns".into(), cols.into());
        if !rels.is_empty() {
            doc.insert("relationships".into(), rels.into());
        }
        table_docs.insert(name.clone(), serde_json::Value::Object(doc));
    }

    // ── input pins ──────────────────────────────────────────────────────
    let authored_hashes: BTreeMap<String, String> = authored
        .iter()
        .map(|(rel, bytes)| (rel.clone(), sha256(bytes)))
        .collect();
    let inputs = serde_json::json!({
        "authored": authored_hashes,
        "platform_bundle": {
            "source": PLATFORM_BUNDLE_SOURCE,
            "hash": bundle_hash(),
        },
        "archetypes": archetypes_hash(),
    });

    // ── content-address and assemble ────────────────────────────────────
    let body = serde_json::json!({ "inputs": inputs, "tables": table_docs });
    let body_text = serde_json::to_string_pretty(&body).context("serialize snapshot body")?;
    let content_hash = sha256(body_text.as_bytes());
    let doc = serde_json::json!({
        "forge_schema_lock": 1,
        "content_hash": content_hash,
        "inputs": body["inputs"],
        "tables": body["tables"],
    });
    let mut text = serde_json::to_string_pretty(&doc).context("serialize snapshot")?;
    text.push('\n');
    Ok(CompiledSnapshot {
        text,
        content_hash,
        tables: tables.len(),
        authored: authored.len(),
        bundle: PLATFORM_BUNDLE.len(),
    })
}

/// The wire shape of a resolved column type — the vocabulary the checker's
/// rule zero judges `.get`s by (`forge-lang-tree`'s `Wire`). Kept in exact
/// step with what storage serialization does to each `ForgeType`, and with
/// the checker's historical treatment of the authored spellings.
/// A column's `enum` key: `{"name", "values"}` for a named enum resolved
/// through the schema's `enums`, `{"values"}` for an inline one, in
/// declaration order. `None` for no enum and for a name that does not
/// resolve.
fn enum_doc(enum_ref: Option<&EnumRefOrInline>, enums: &[EnumDef]) -> Option<serde_json::Value> {
    match enum_ref? {
        EnumRefOrInline::Inline(values) => Some(serde_json::json!({ "values": values })),
        EnumRefOrInline::Named(name) => {
            let def = enums.iter().find(|e| &e.name == name)?;
            let values: Vec<&str> = def
                .values
                .iter()
                .map(|v| match v {
                    EnumValue::Simple(s) => s.as_str(),
                    EnumValue::Labeled { value, .. } => value.as_str(),
                })
                .collect();
            Some(serde_json::json!({ "name": name, "values": values }))
        }
    }
}

fn wire_of(ft: ForgeType) -> &'static str {
    match ft {
        ForgeType::Text | ForgeType::Uuid => "text",
        ForgeType::Timestamp | ForgeType::Date | ForgeType::Time => "timestamp",
        ForgeType::Int32 | ForgeType::Int64 | ForgeType::Float64 => "number",
        ForgeType::Bool => "bool",
        ForgeType::Json => "object",
        // Duration, Bytes, Vector, GeoPoint, and anything types.yaml grows:
        // the column exists, its reads are not type-checked.
        _ => "unknown",
    }
}

/// The archetype catalog's pin: the generated `ArchetypeDescriptor` list
/// (types.yaml → forge-types build.rs), serialized. A types.yaml archetype
/// change moves this hash, which fails the drift gate everywhere at once —
/// the point.
fn archetypes_hash() -> String {
    let catalog = forge_types::SubstrateDescriptor::current("schema-lock");
    let text = serde_json::to_string(&catalog.archetypes).expect("archetype catalog serializes");
    sha256(text.as_bytes())
}

// ── `forge schema op new` (ST-6 decision 4) ─────────────────────────────

use forge_lang_rustgen::scaffold;

/// Everything `op new` checks before it writes, then the two writes: the
/// `service.json` entry and the handler stub. Answers the paths written.
/// Nothing is written when any check refuses.
fn op_new(root: &Path, args: &OpNewArgs) -> Result<Vec<PathBuf>> {
    if !root.join("workspace.json").exists() {
        bail!(
            "no workspace.json at {} — `forge schema op new` writes into a \
             workspace; pass --manifest-dir or run from the workspace root",
            root.display()
        );
    }
    let Some((domain, op)) = args.target.split_once("::") else {
        bail!("`{}` is not `<domain>::<op>`", args.target);
    };
    for (what, name) in [("domain", domain), ("op", op)] {
        if !is_snake_ident(name) {
            bail!("the {what} `{name}` is not a snake_case name (`[a-z][a-z0-9_]*`)");
        }
    }
    if !matches!(args.kind.as_str(), "query" | "mutation") {
        bail!(
            "unknown --kind `{}` (expected `query` or `mutation`)",
            args.kind
        );
    }
    let dir = root.join("domains").join(domain);
    let service_path = dir.join("service.json");
    let Ok(text) = std::fs::read_to_string(&service_path) else {
        bail!(
            "unknown domain `{domain}`: no {} — the domains are {}",
            service_path.display(),
            known_domains(root)
        );
    };
    if dialect::declared_ops(&dir)
        .unwrap_or_default()
        .iter()
        .any(|o| o == op)
    {
        bail!(
            "`{domain}::{op}` already exists in {}",
            service_path.display()
        );
    }
    let lang = match &args.lang {
        Some(word) => scaffold::Lang::parse(word)
            .with_context(|| format!("unknown --lang `{word}` (one of py, ts, java, rust)"))?,
        None => domain_lang(&dir, domain)?,
    };
    let (stub_rel, stub) = scaffold::handler_stub(lang, domain, op);
    let stub_path = root.join(&stub_rel);
    if stub_path.exists() {
        bail!(
            "{} already exists — `op new` never overwrites a handler",
            stub_path.display()
        );
    }
    let appended = append_op(&text, &scaffold::service_entry(op, &args.kind))
        .with_context(|| format!("append `{op}` to {}", service_path.display()))?;

    std::fs::write(&service_path, appended)
        .with_context(|| format!("write {}", service_path.display()))?;
    if let Some(parent) = stub_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    std::fs::write(&stub_path, stub).with_context(|| format!("write {}", stub_path.display()))?;
    Ok(vec![service_path, stub_path])
}

fn is_snake_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The workspace's domains, for a refusal that names what does exist.
fn known_domains(root: &Path) -> String {
    let names: Vec<String> = dialect::domains(root)
        .iter()
        .filter(|d| d.join("service.json").is_file())
        .map(|d| format!("`{}`", dialect::domain_name(d)))
        .collect();
    match names.is_empty() {
        true => "none yet".to_string(),
        false => names.join(", "),
    }
}

/// The language of the domain's existing ops, when they share one.
fn domain_lang(dir: &Path, domain: &str) -> Result<scaffold::Lang> {
    use forge_lang_rustgen::Lang;
    let mut langs: Vec<scaffold::Lang> = dialect::sources_of(dir)
        .iter()
        .filter_map(|s| forge_lang_rustgen::lang_of(s))
        .map(|l| match l {
            Lang::Python => scaffold::Lang::Py,
            Lang::TypeScript => scaffold::Lang::Ts,
            Lang::Java => scaffold::Lang::Java,
            Lang::Rust => scaffold::Lang::Rust,
        })
        .collect();
    langs.dedup();
    match langs.as_slice() {
        [one] => Ok(*one),
        [] => bail!(
            "domain `{domain}` has no ops to take a language from: pass --lang py|ts|java|rust"
        ),
        _ => bail!(
            "domain `{domain}` holds ops in more than one language: pass --lang py|ts|java|rust"
        ),
    }
}

/// `text` with `entry` appended to its top-level `operations` array, every
/// other byte as it was: the file's key order, indent and formatting are
/// the author's, and re-serialising the whole document would reorder its
/// keys. The entry is written in the file's indent unit, its keys in the
/// order the file's first op uses. The result is parsed back and compared
/// with the intended document before it is answered.
fn append_op(text: &str, entry: &serde_json::Value) -> Result<String> {
    let mut doc: serde_json::Value = serde_json::from_str(text).context("not valid JSON")?;
    let Some(ops) = doc.get_mut("operations").and_then(|o| o.as_array_mut()) else {
        bail!("no top-level `operations` array to append to");
    };
    ops.push(entry.clone());
    let (open, close) = operations_span(text).context("no top-level `operations` array")?;
    let unit = indent_unit(text);
    let order = first_op_keys(text);
    let rank = |key: &str| -> (usize, usize) {
        const DEFAULT: [&str; 8] = [
            "name",
            "kind",
            "type",
            "properties",
            "required",
            "input_schema",
            "output_schema",
            "errors",
        ];
        match order.iter().position(|k| k == key) {
            Some(i) => (0, i),
            None => (
                1,
                DEFAULT
                    .iter()
                    .position(|k| *k == key)
                    .unwrap_or(DEFAULT.len()),
            ),
        }
    };
    let mut rendered = String::new();
    write_json(entry, &unit, 2, &rank, &mut rendered);
    let inner = &text[open + 1..close];
    let head = text[..close].trim_end();
    let sep = match inner.trim().is_empty() {
        true => "",
        false => ",",
    };
    let out = format!(
        "{head}{sep}\n{}{rendered}\n{unit}{}",
        unit.repeat(2),
        &text[close..]
    );
    let back: serde_json::Value = serde_json::from_str(&out).context("the appended file")?;
    if back != doc {
        bail!("the appended file does not read back as the intended document");
    }
    Ok(out)
}

/// The byte offsets of the top-level `operations` array's `[` and `]`.
fn operations_span(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    let mut last_key: Option<(usize, usize)> = None;
    let mut open = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                last_key = Some((start + 1, i));
            }
            b'{' | b'[' => {
                depth += 1;
                if bytes[i] == b'['
                    && depth == 2
                    && open.is_none()
                    && last_key.is_some_and(|(a, b)| &text[a..b] == "operations")
                {
                    open = Some(i);
                }
            }
            b'}' | b']' => {
                if depth == 2 && bytes[i] == b']' && open.is_some() {
                    return open.map(|o| (o, i));
                }
                depth = depth.saturating_sub(1);
            }
            b',' => last_key = None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The file's indent unit: the leading whitespace of its first indented
/// line, else two spaces.
fn indent_unit(text: &str) -> String {
    text.lines()
        .map(|l| &l[..l.len() - l.trim_start().len()])
        .find(|ws| !ws.is_empty())
        .unwrap_or("  ")
        .to_string()
}

/// The keys of the file's first op, in the order the file writes them.
fn first_op_keys(text: &str) -> Vec<String> {
    use serde::Deserialize;
    use serde::de::{IgnoredAny, MapAccess, Visitor};

    struct Keys(Vec<String>);
    impl<'de> Deserialize<'de> for Keys {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Keys, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        map.next_value::<IgnoredAny>()?;
                        keys.push(key);
                    }
                    Ok(Keys(keys))
                }
            }
            d.deserialize_map(V)
        }
    }
    #[derive(Deserialize)]
    struct Service {
        #[serde(default)]
        operations: Vec<Keys>,
    }
    serde_json::from_str::<Service>(text)
        .ok()
        .and_then(|s| s.operations.into_iter().next())
        .map(|k| k.0)
        .unwrap_or_default()
}

/// `value` as pretty JSON at `level` indents of `unit`, object keys ordered
/// by `rank` — the layout `serde_json::to_string_pretty` writes, with the
/// file's indent and key order in place of its own.
fn write_json(
    value: &serde_json::Value,
    unit: &str,
    level: usize,
    rank: &dyn Fn(&str) -> (usize, usize),
    out: &mut String,
) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|k| (rank(k), (*k).clone()));
            out.push_str("{\n");
            for (i, key) in keys.iter().enumerate() {
                out.push_str(&unit.repeat(level + 1));
                out.push_str(&serde_json::to_string(key).expect("a string"));
                out.push_str(": ");
                write_json(&map[key.as_str()], unit, level + 1, rank, out);
                out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
            }
            out.push_str(&unit.repeat(level));
            out.push('}');
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&unit.repeat(level + 1));
                write_json(item, unit, level + 1, rank, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&unit.repeat(level));
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("plain data")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny workspace: one domain, one Record table riding a relationship
    /// into a bundle table, plus the converge directive the apply strips.
    fn workspace() -> tempfile::TempDir {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("workspace.json"), "{}").unwrap();
        let schemas = ws.path().join("domains/d/schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        std::fs::write(
            schemas.join("widgets.table.json"),
            r#"{"name":"widgets","archetype":"Record",
                "label":"Widget","plural_label":"Widgets","header_fields":["title"],
                "columns":[{"name":"title","type":"string"},
                           {"name":"count","type":"integer","required":false}],
                "relationships":[{"name":"actor","target_table":"audit_events",
                                  "local_key":"created_by"},
                                 {"name":"parts","target_table":"widget_parts",
                                  "foreign_key":"widget_id","parent_key":"id"}],
                "accept_destructive": true}"#,
        )
        .unwrap();
        std::fs::write(
            schemas.join("widget_parts.table.json"),
            r#"{"name":"widget_parts","archetype":"Base",
                "label":"Part","plural_label":"Parts","header_fields":["widget_id"],
                "columns":[{"name":"widget_id","type":"string"}]}"#,
        )
        .unwrap();
        ws
    }

    #[test]
    fn compile_is_deterministic_and_materializes_system_columns() {
        let ws = workspace();
        let a = compile_snapshot(ws.path()).unwrap();
        let b = compile_snapshot(ws.path()).unwrap();
        assert_eq!(
            a.text, b.text,
            "two compiles of one tree must be byte-equal"
        );
        assert_eq!(a.authored, 2);
        assert_eq!(a.bundle, PLATFORM_BUNDLE.len());

        let doc: serde_json::Value = serde_json::from_str(&a.text).unwrap();
        let widgets = &doc["tables"]["widgets"];
        assert_eq!(widgets["archetype"], "Record");
        let cols: Vec<(&str, &str, bool)> = widgets["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap(),
                    c["wire"].as_str().unwrap(),
                    c["system"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        // The Record archetype's five system columns, from the generated
        // catalog, then the authored two — exactly what the apply builds.
        assert_eq!(
            cols,
            vec![
                ("id", "text", true),
                ("created_at", "timestamp", true),
                ("created_by", "text", true),
                ("last_modified_at", "timestamp", true),
                ("last_modified_by", "text", true),
                ("title", "text", false),
                ("count", "number", false),
            ]
        );
        // The relationship resolved against the BUNDLE table — the by-ref
        // pin doing its job — and the runtime-owned set is present.
        assert_eq!(widgets["relationships"][0]["target_table"], "audit_events");
        // Both kinds, each with the key it was read off (typed-schema TS-7).
        // A consumer typing a followed read needs the kind, because a
        // has-many arrives as a nested array under its own key and a
        // belongs-to arrives flattened onto the parent with dotted keys.
        assert_eq!(widgets["relationships"][0]["kind"], "belongs_to");
        assert_eq!(widgets["relationships"][0]["local_key"], "created_by");
        assert_eq!(widgets["relationships"][1]["name"], "parts");
        assert_eq!(widgets["relationships"][1]["kind"], "has_many");
        assert_eq!(widgets["relationships"][1]["foreign_key"], "widget_id");
        assert_eq!(widgets["relationships"][1]["parent_key"], "id");
        assert!(
            doc["tables"]["audit_events"]["origin"]
                .as_str()
                .unwrap()
                .starts_with("platform-bundle:")
        );
        // The parse is the apply's parse: `accept_destructive` was stripped
        // as a directive, not rejected as an unknown field.
        assert_eq!(doc["forge_schema_lock"], 1);
    }

    #[test]
    fn has_default_marks_only_defaulted_columns() {
        let ws = workspace();
        std::fs::write(
            ws.path().join("domains/d/schemas/gadgets.table.json"),
            r#"{"name":"gadgets","archetype":"Base",
                "label":"Gadget","plural_label":"Gadgets","header_fields":["label"],
                "columns":[{"name":"status","type":"string","default":"new"},
                           {"name":"label","type":"string"}]}"#,
        )
        .unwrap();
        let compiled = compile_snapshot(ws.path()).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&compiled.text).unwrap();
        let cols = doc["tables"]["gadgets"]["columns"].as_array().unwrap();
        let col = |n: &str| cols.iter().find(|c| c["name"] == n).unwrap();
        // Both NOT NULL; only the defaulted one carries the mark, and the
        // default's value never enters the lock.
        assert_eq!(col("status")["nullable"], false);
        assert_eq!(col("label")["nullable"], false);
        assert_eq!(col("status")["has_default"], true);
        assert!(col("label").get("has_default").is_none());
        assert!(!compiled.text.contains("\"new\""), "{}", compiled.text);

        // A table with no defaults serializes exactly as before the field
        // existed: the key is omitted, never written as `false`, so such a
        // lock keeps its bytes and its content_hash.
        let parts = serde_json::to_string_pretty(&doc["tables"]["widget_parts"]).unwrap();
        assert!(!parts.contains("has_default"), "{parts}");
        assert!(!compiled.text.contains("\"has_default\": false"));
    }

    #[test]
    fn enum_and_picklist_record_only_their_closed_sets() {
        let ws = workspace();
        std::fs::write(
            ws.path().join("domains/d/schemas/tickets.table.json"),
            r#"{"name":"tickets","archetype":"Base",
                "label":"Ticket","plural_label":"Tickets","header_fields":["title"],
                "enums":[{"name":"ticket_state","values":["open",
                          {"value":"closed","label":"Closed for good"}]}],
                "columns":[{"name":"state","type":"string","enum":"ticket_state"},
                           {"name":"rating","type":"string","enum":["hot","cold"]},
                           {"name":"ghost","type":"string","enum":"no_such_enum"},
                           {"name":"size","type":"string",
                            "values":[{"value":"s","display":"Small"},{"value":"l"}]},
                           {"name":"title","type":"string"}]}"#,
        )
        .unwrap();
        let compiled = compile_snapshot(ws.path()).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&compiled.text).unwrap();
        let cols = doc["tables"]["tickets"]["columns"].as_array().unwrap();
        let col = |n: &str| cols.iter().find(|c| c["name"] == n).unwrap();
        // Named: resolved through `enums`, labelled values by their value.
        assert_eq!(
            col("state")["enum"],
            serde_json::json!({"name": "ticket_state", "values": ["open", "closed"]})
        );
        // Inline: values only, no name.
        assert_eq!(
            col("rating")["enum"],
            serde_json::json!({"values": ["hot", "cold"]})
        );
        // Unresolved name: storage enforces nothing, so no key.
        assert!(col("ghost").get("enum").is_none());
        // Picklist: values in order, display text never written.
        assert_eq!(col("size")["picklist"], serde_json::json!(["s", "l"]));
        assert!(!compiled.text.contains("Small"), "{}", compiled.text);
        assert!(
            !compiled.text.contains("Closed for good"),
            "{}",
            compiled.text
        );
        // A plain column carries neither key.
        assert!(col("title").get("enum").is_none());
        assert!(col("title").get("picklist").is_none());
        let parts = serde_json::to_string_pretty(&doc["tables"]["widget_parts"]).unwrap();
        assert!(
            !parts.contains("\"enum\"") && !parts.contains("picklist"),
            "{parts}"
        );
    }

    #[test]
    fn an_unresolved_relationship_refuses_the_compile() {
        let ws = workspace();
        std::fs::write(
            ws.path().join("domains/d/schemas/orphans.table.json"),
            r#"{"name":"orphans","archetype":"Base",
                "label":"Orphan","plural_label":"Orphans","header_fields":["x"],
                "columns":[{"name":"x","type":"string"}],
                "relationships":[{"name":"ghost","target_table":"no_such_table"}]}"#,
        )
        .unwrap();
        let err = compile_snapshot(ws.path()).unwrap_err().to_string();
        assert!(err.contains("no_such_table"), "{err}");
        assert!(err.contains("orphans"), "{err}");
    }

    #[test]
    fn check_prefers_a_written_snapshot_and_refuses_a_stale_one() {
        let ws = workspace();
        let compiled = compile_snapshot(ws.path()).unwrap();
        std::fs::write(
            ws.path().join(forge_lang_rustgen::SNAPSHOT_FILE),
            &compiled.text,
        )
        .unwrap();

        let (tables, hash) = dialect::workspace_tables(ws.path()).unwrap();
        let tables = tables.unwrap();
        assert_eq!(hash.as_deref(), Some(compiled.content_hash.as_str()));
        // Runtime-owned tables are visible with NO platform-schemas/ dir.
        assert!(tables.get("user_preferences").is_some());
        // System columns exist; an undeclared one does not (closed).
        assert!(
            tables
                .get("widgets")
                .unwrap()
                .column("created_by")
                .is_some()
        );
        assert!(tables.get("widgets").unwrap().column("nope").is_none());

        // Doctor an input: stale, named, refused.
        std::fs::write(
            ws.path().join("domains/d/schemas/widgets.table.json"),
            r#"{"name":"widgets","archetype":"Base",
                "label":"Widget","plural_label":"Widgets","header_fields":["title"],
                "columns":[{"name":"title","type":"string"}]}"#,
        )
        .unwrap();
        let err = dialect::workspace_tables(ws.path()).unwrap_err();
        assert!(err.contains("stale"), "{err}");
        assert!(err.contains("widgets.table.json"), "{err}");
    }

    // ── `forge schema op new` and the editor schema (ST-6) ──────────────

    /// A fresh `forge new --template workspace --dialect <lang>` tree.
    fn dialect_workspace(lang: forge_lang_rustgen::Lang) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("acme");
        crate::cmd::new::write_dialect_tree(&dir, "acme", "acme", lang).unwrap();
        (tmp, dir)
    }

    fn op_new_args(root: &Path, target: &str, lang: Option<&str>) -> OpNewArgs {
        OpNewArgs {
            target: target.to_string(),
            lang: lang.map(str::to_string),
            kind: "mutation".to_string(),
            manifest_dir: Some(root.to_path_buf()),
        }
    }

    /// The four languages, with the stub path `op new` writes for
    /// `acme::plan_refund`.
    const STUBS: [(forge_lang_rustgen::Lang, &str, &str); 4] = [
        (
            forge_lang_rustgen::Lang::Python,
            "py",
            "domains/acme/services/plan_refund.py",
        ),
        (
            forge_lang_rustgen::Lang::TypeScript,
            "ts",
            "domains/acme/services/plan_refund.ts",
        ),
        (
            forge_lang_rustgen::Lang::Java,
            "java",
            "domains/acme/services/PlanRefund.java",
        ),
        (
            forge_lang_rustgen::Lang::Rust,
            "rust",
            "domains/acme/services/plan_refund.rs",
        ),
    ];

    /// For each language: `op new` in a workspace from the `forge new`
    /// template declares the op after the starter and writes its stub; a
    /// second `op new` of the same op is refused, naming it.
    #[test]
    fn op_new_declares_the_op_writes_its_stub_and_refuses_a_second() {
        for (lang, word, stub) in STUBS {
            let (_tmp, dir) = dialect_workspace(lang);
            let args = op_new_args(&dir, "acme::plan_refund", Some(word));
            let written = op_new(&dir, &args).unwrap_or_else(|e| panic!("{word}: {e:#}"));
            assert_eq!(written[1], dir.join(stub), "{word}");
            let (_, text) =
                scaffold::handler_stub(scaffold::Lang::parse(word).unwrap(), "acme", "plan_refund");
            assert_eq!(std::fs::read_to_string(&written[1]).unwrap(), text);
            let declared = crate::dialect::declared_ops(&dir.join("domains/acme")).unwrap();
            assert_eq!(declared, ["hello", "plan_refund"], "{word}");

            let again = op_new(&dir, &args).expect_err("a second `op new` of the same op");
            assert!(
                format!("{again:#}").contains("`acme::plan_refund` already exists"),
                "{word}: {again:#}"
            );
        }
    }

    /// ST-6 decision 4: after `op new`, `forge check` over the workspace is
    /// green in every language. It is not on the forge-lang this lane links:
    /// `forge check` stages no generated surface, the scoped contract
    /// surfaces do not make an empty declared request or response a known
    /// type (py FL0014, ts FL1053), and javac and the Rust gate cannot
    /// resolve `forge.schema` / `forge::schema` at all. The same stubs are
    /// accepted with `$FORGE_LANG_SCHEMA_{PY,TS,RS}` pointed at `.forge/types`.
    /// ST-6 slice 2 core closes it in forge-lang: the contract-scoped check
    /// generates the surface from the scoped `service.json` itself, so the
    /// input stays `service.json` and never the editor's `.forge/types`.
    #[test]
    #[ignore = "ST-6 slice 2 core: check_only_with generates the surface from the scoped service.json when none is staged"]
    fn op_new_then_forge_check_is_green_in_every_language() {
        let mut refused: Vec<String> = Vec::new();
        for (lang, word, _) in STUBS {
            let (_tmp, dir) = dialect_workspace(lang);
            op_new(&dir, &op_new_args(&dir, "acme::plan_refund", Some(word)))
                .unwrap_or_else(|e| panic!("{word}: {e:#}"));
            refresh_types(&dir);
            match crate::cmd::check::check(&dir) {
                Ok(found) if found.ok() => assert_eq!(found.registers.len(), 2, "{word}"),
                Ok(found) => {
                    let text: String = found.registers.iter().map(|r| r.render()).collect();
                    refused.push(format!("{word}:\n{text}"));
                }
                Err(e) => refused.push(format!("{word}: the checker could not run: {e}")),
            }
        }
        assert!(
            refused.is_empty(),
            "`forge check` refused the scaffold:\n{}",
            refused.join("\n")
        );
    }

    /// With no `--lang`, the stub takes the domain's existing ops' language;
    /// an unknown domain is refused naming it and the domains there are.
    #[test]
    fn op_new_defaults_the_language_and_refuses_an_unknown_domain() {
        let (_tmp, dir) = dialect_workspace(forge_lang_rustgen::Lang::TypeScript);
        let written = op_new(&dir, &op_new_args(&dir, "acme::list_plans", None)).unwrap();
        assert_eq!(written[1], dir.join("domains/acme/services/list_plans.ts"));

        let err = op_new(&dir, &op_new_args(&dir, "billing::charge", None)).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("unknown domain `billing`"), "{err}");
        assert!(err.contains("`acme`"), "{err}");
    }

    /// The append keeps every byte of the file the author wrote — its key
    /// order and its indent — and writes the new entry in both.
    #[test]
    fn the_append_keeps_the_files_key_order_and_indent() {
        let text = "{\n    \"name\": \"billing\",\n    \"domain\": \"billing\",\n    \
                    \"operations\": [\n        {\n            \"name\": \"charge\",\n            \
                    \"kind\": \"mutation\",\n            \"summary\": \"Charge.\"\n        }\n    ]\n}\n";
        let entry = scaffold::service_entry("refund", "query");
        let out = append_op(text, &entry).unwrap();
        let head = "{\n    \"name\": \"billing\",\n    \"domain\": \"billing\",\n    \
                    \"operations\": [\n        {\n            \"name\": \"charge\",\n            \
                    \"kind\": \"mutation\",\n            \"summary\": \"Charge.\"\n        },\n";
        assert!(out.starts_with(head), "{out}");
        assert_eq!(
            &out[head.len()..],
            "        {\n            \"name\": \"refund\",\n            \"kind\": \"query\",\n            \
             \"input_schema\": {\n                \"type\": \"object\",\n                \
             \"properties\": {}\n            },\n            \"output_schema\": {\n                \
             \"type\": \"object\",\n                \"properties\": {}\n            },\n            \
             \"errors\": []\n        }\n    ]\n}\n"
        );
    }

    /// A fresh `forge new` and `forge schema compile` leave the editor both
    /// halves: `.forge/types/service.schema.json`, and the committed
    /// `.vscode/settings.json` that maps every domain's `service.json` to it.
    #[test]
    fn a_fresh_workspace_maps_service_json_to_its_json_schema() {
        let (_tmp, dir) = dialect_workspace(forge_lang_rustgen::Lang::Python);
        compile(CompileArgs {
            manifest_dir: Some(dir.clone()),
            check: false,
        })
        .unwrap();
        let schema = dir
            .join(forge_lang_rustgen::workspace_types::TYPES_DIR)
            .join(SERVICE_SCHEMA_FILE);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&schema).unwrap()).unwrap();
        assert_eq!(written, forge_lang_rustgen::service_json_schema());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".vscode/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            settings["json.schemas"],
            serde_json::json!([{
                "fileMatch": ["domains/*/service.json"],
                "url": "./.forge/types/service.schema.json",
            }])
        );
    }
}
