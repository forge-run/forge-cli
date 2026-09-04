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
//! target either resolves or the compile refuses.
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
    Ok(())
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
            rels.push(serde_json::json!({
                "name": rel.name,
                "target_table": target,
            }));
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
                                  "local_key":"created_by"}],
                "accept_destructive": true}"#,
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
        assert_eq!(a.authored, 1);
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
}
