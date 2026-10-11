//! `forge export` — write every table the caller's identity can read as
//! `<table>.jsonl`, plus a `manifest.json` saying what the export is and what
//! it is not.
//!
//! It is a read under the caller's own bearer, the same one every other
//! command sends, so storage applies the caller's RLS exactly as it does for
//! `GET /api/v1/data/{table}`: the export is what this identity can read, not
//! a workspace backup. It never sends an admin (`fr_a_`) or platform (`fr_p_`)
//! bearer — a configured one is refused before any request leaves.
//!
//! The table list is the live workspace's OpenAPI document (every
//! `/api/v1/data/{table}` with a GET). That document needs `platform:read`,
//! which a workspace-local user lacks, so for such a caller the list is the
//! tables the config tree declares, each probed through its data route. Rows
//! are that route's pages. The first
//! page read answers the MVCC sequence it read at as `x-forge-as-of`, and
//! every later page of every table sends it back, so the whole export is one
//! consistent cut. A runtime that answers no sequence is refused rather than
//! exported unpinned. What has no route — `api.access: hidden` tables, the
//! platform's `_` tables, blobs — is named in the manifest as not exported, so
//! the manifest never implies a completeness the routes cannot give.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;
use forge_types::schema::SchemaDefinition;
use forge_types::schema::metadata::ApiAccess;
use reqwest::header::HeaderMap;

use crate::client::{ForgeClient, ForgeError};
use crate::dialect;

/// The read-pin header, both directions (forge-runtime
/// `http/handler/dispatch.rs` `AS_OF_HEADER`).
const AS_OF: &str = "x-forge-as-of";
const HAS_MORE: &str = "x-forge-has-more";
const RLS: &str = "x-forge-rls";
/// The CRUD list route's page cap (forge-registry `list_input_schema`).
const MAX_PAGE: usize = 1000;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Directory to write into. Created if absent; refused if it already holds
    /// anything, so two exports never mix in one directory.
    #[arg(long)]
    pub out: PathBuf,
    /// The workspace config tree, read for the manifest's git sha and for the
    /// tables it declares that the live workspace serves no route for.
    /// Defaults to the current directory.
    #[arg(long)]
    pub manifest_dir: Option<PathBuf>,
    /// Rows per page, at most 1000 (the list route's cap).
    #[arg(long, default_value_t = MAX_PAGE, hide = true)]
    pub page_size: usize,
}

pub async fn run(args: ExportArgs, client: &ForgeClient) -> Result<()> {
    let manifest = export(&args, client).await?;
    let exported: Vec<_> = manifest.tables.iter().filter(|t| t.exported).collect();
    let rows: u64 = exported.iter().map(|t| t.rows).sum();
    eprintln!(
        "export: ✅ {} table(s), {rows} row(s) as of sequence {} → {}",
        exported.len(),
        manifest.as_of,
        args.out.display()
    );
    for t in manifest.tables.iter().filter(|t| t.note.is_some()) {
        eprintln!("  {}: {}", t.name, t.note.as_deref().unwrap_or_default());
    }
    eprintln!("export: the rows are what this identity can read — not a workspace backup");
    Ok(())
}

#[derive(Debug, serde::Serialize)]
struct Manifest {
    format: &'static str,
    exported_at: String,
    workspace_url: String,
    scope: &'static str,
    /// The MVCC sequence every page of every table was read at.
    as_of: u64,
    config_tree: ConfigTree,
    /// Where the table list came from: the workspace's OpenAPI document, or —
    /// when that refuses this identity — the tables the config tree declares.
    tables_from: String,
    tables: Vec<TableEntry>,
    blobs: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct ConfigTree {
    path: String,
    /// `HEAD` of the config tree's git checkout; `None` outside a checkout.
    git_sha: Option<String>,
    /// Uncommitted changes in the tree, so `git_sha` alone does not name it.
    dirty: bool,
}

#[derive(Debug, serde::Serialize)]
struct TableEntry {
    name: String,
    exported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

async fn export(args: &ExportArgs, client: &ForgeClient) -> Result<Manifest> {
    refuse_privileged_bearer(&client.token())?;
    if args.page_size == 0 || args.page_size > MAX_PAGE {
        bail!("--page-size must be 1..={MAX_PAGE}");
    }
    prepare_out_dir(&args.out)?;
    let tree = args
        .manifest_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let declared = declared_tables(&tree);

    // The OpenAPI document needs `platform:read`, which a federated (portal)
    // bearer carries and a workspace-local user does not. Such a caller still
    // reads its rows through the data routes, so the table list falls back to
    // what the config tree declares, each probed through its own list route.
    let (routed, tables_from) = match client
        .get_json::<serde_json::Value>("/api/v1/platform/openapi")
        .await
    {
        Ok(spec) => (routed_tables(&spec), "openapi".to_string()),
        Err(ForgeError::Http { status, body }) if status.as_u16() == 403 => {
            let probed: BTreeSet<String> = declared
                .iter()
                .filter(|(_, d)| d.access != Some(ApiAccess::Hidden))
                .map(|(name, _)| name.clone())
                .collect();
            if probed.is_empty() {
                bail!(
                    "GET /api/v1/platform/openapi refused this identity ({}), and {} declares \
                     no tables to probe instead — run from the workspace's config tree or \
                     pass --manifest-dir",
                    body.trim(),
                    tree.display()
                );
            }
            let why = format!(
                "config tree: the OpenAPI document refused this identity ({}), so the list is \
                 the tables {} declares; runtime-owned tables are not covered",
                body.trim(),
                tree.display()
            );
            (probed, why)
        }
        Err(e) => return Err(http_err("GET /api/v1/platform/openapi", e)),
    };

    let mut pin: Option<u64> = None;
    let mut tables = Vec::new();
    for name in &routed {
        if name.starts_with('_') {
            tables.push(skipped(name, "platform table: not exported"));
            continue;
        }
        let mut entry = export_table(client, &args.out, name, args.page_size, &mut pin).await?;
        if let Some(hidden) = declared.get(name).map(|d| &d.hidden_columns)
            && !hidden.is_empty()
        {
            let cols = format!(
                "columns hidden from the API are not in it: {}",
                hidden.join(", ")
            );
            entry.note = Some(match entry.note {
                Some(n) => format!("{n}; {cols}"),
                None => cols,
            });
        }
        tables.push(entry);
    }
    for (name, d) in &declared {
        if routed.contains(name) {
            continue;
        }
        tables.push(skipped(
            name,
            if d.access == Some(ApiAccess::Hidden) {
                "hidden: api.access is hidden, so the table has no data route"
            } else {
                "unrouted: declared in the config tree, but the live workspace serves no \
                 list route for it (not converged yet, or applied elsewhere)"
            },
        ));
    }
    let Some(as_of) = pin else {
        bail!("the workspace serves no data route this export could read — nothing to pin");
    };

    let (git_sha, dirty) = git_state(&tree);
    let manifest = Manifest {
        format: "forge-export/1",
        exported_at: rfc3339_now(),
        workspace_url: client.base_url(),
        scope: "the rows the caller's identity can read, under its own RLS; not a workspace backup",
        as_of,
        config_tree: ConfigTree {
            path: tree.display().to_string(),
            git_sha,
            dirty,
        },
        tables_from,
        tables,
        blobs: "not exported: blobs have no data route",
    };
    let path = args.out.join("manifest.json");
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(manifest)
}

fn skipped(name: &str, note: &str) -> TableEntry {
    TableEntry {
        name: name.to_string(),
        exported: false,
        file: None,
        rows: 0,
        note: Some(note.to_string()),
    }
}

/// Walk one table's pages at the pinned sequence, writing each row once.
/// The first page of the whole export sets `pin`; every request after it
/// carries it, and a page answering a different sequence is refused.
async fn export_table(
    client: &ForgeClient,
    out: &Path,
    name: &str,
    page_size: usize,
    pin: &mut Option<u64>,
) -> Result<TableEntry> {
    let file_name = format!("{name}.jsonl");
    let path = out.join(&file_name);
    let mut writer = std::io::BufWriter::new(
        std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?,
    );
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut rows: u64 = 0;
    let mut offset: usize = 0;
    let mut rls_note = None;
    loop {
        let route = format!("/api/v1/data/{name}?limit={page_size}&offset={offset}");
        let extra: Vec<(String, String)> = pin
            .map(|seq| vec![(AS_OF.to_string(), seq.to_string())])
            .unwrap_or_default();
        let (headers, body) = match client.get_with_headers(&route, &extra).await {
            Ok(r) => r,
            Err(ForgeError::Http { status, body }) if status.as_u16() == 403 => {
                drop(writer);
                std::fs::remove_file(&path).ok();
                return Ok(skipped(
                    name,
                    &format!(
                        "refused: the list route answered 403 for this identity: {}",
                        body.trim()
                    ),
                ));
            }
            Err(ForgeError::Http { status, .. }) if status.as_u16() == 404 && offset == 0 => {
                drop(writer);
                std::fs::remove_file(&path).ok();
                return Ok(skipped(
                    name,
                    "unrouted: declared in the config tree, but the live workspace serves no \
                     list route for it (not converged yet, or applied elsewhere)",
                ));
            }
            Err(e) => return Err(http_err(&format!("GET {route}"), e)),
        };
        let answered = header(&headers, AS_OF)
            .map(|v| {
                v.parse::<u64>()
                    .with_context(|| format!("{name}: {AS_OF} `{v}` is not a sequence"))
            })
            .transpose()?;
        match (*pin, answered) {
            (_, None) => bail!(
                "{name}: the runtime answered no {AS_OF} header, so the export cannot pin one \
                 consistent cut; this workspace's runtime predates the read pin"
            ),
            (None, Some(seq)) => *pin = Some(seq),
            (Some(want), Some(got)) if want != got => {
                bail!("{name}: a page read at sequence {got}, not the pinned {want}")
            }
            _ => {}
        }
        if let Some(marker) = header(&headers, RLS) {
            rls_note = Some(format!(
                "rls-denied ({marker}): this identity can read no rows of this table"
            ));
        }
        let page: Vec<serde_json::Value> = serde_json::from_slice(&body)
            .with_context(|| format!("{name}: a list page is not a JSON row array"))?;
        let n = page.len();
        for row in &page {
            if let Some(id) = row.get("id")
                && !seen_ids.insert(id.to_string())
            {
                bail!("{name}: row id {id} was answered twice — the walk is not one snapshot");
            }
            serde_json::to_writer(&mut writer, row)?;
            writer.write_all(b"\n")?;
        }
        rows += n as u64;
        offset += n;
        let more = match header(&headers, HAS_MORE) {
            Some(v) => v == "true",
            None => n == page_size,
        };
        if !more || n == 0 {
            break;
        }
    }
    writer
        .flush()
        .with_context(|| format!("write {}", path.display()))?;
    Ok(TableEntry {
        name: name.to_string(),
        exported: true,
        file: Some(file_name),
        rows,
        note: rls_note,
    })
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// The tables the live workspace routes: every `/api/v1/data/{table}` path
/// with a GET. A name that could escape the output directory is dropped.
fn routed_tables(spec: &serde_json::Value) -> BTreeSet<String> {
    let Some(paths) = spec.get("paths").and_then(|p| p.as_object()) else {
        return BTreeSet::new();
    };
    paths
        .iter()
        .filter(|(_, item)| item.get("get").is_some())
        .filter_map(|(path, _)| path.strip_prefix("/api/v1/data/"))
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .map(str::to_string)
        .collect()
}

struct Declared {
    access: Option<ApiAccess>,
    hidden_columns: Vec<String>,
}

/// The tables the config tree authors, with their API access and hidden
/// columns. A schema that does not parse is skipped: `forge check` owns
/// that refusal, and the export still answers for what the routes serve.
fn declared_tables(tree: &Path) -> BTreeMap<String, Declared> {
    let mut out = BTreeMap::new();
    for (_, path) in dialect::authored_schema_files(tree) {
        let Some(mut json) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            continue;
        };
        if let Some(obj) = json.as_object_mut() {
            obj.remove("accept_destructive");
        }
        let Ok(schema) =
            SchemaDefinition::from_json(&serde_json::to_vec(&json).unwrap_or_default())
        else {
            continue;
        };
        out.insert(
            schema.name().to_string(),
            Declared {
                access: schema.api().and_then(|a| a.access),
                hidden_columns: schema.api().map(|a| a.hidden.clone()).unwrap_or_default(),
            },
        );
    }
    out
}

fn refuse_privileged_bearer(token: &str) -> Result<()> {
    for (prefix, kind) in [("fr_a_", "an admin"), ("fr_p_", "a platform")] {
        if token.starts_with(prefix) {
            bail!(
                "the configured bearer is {kind} token ({prefix}…); `forge export` reads as \
                 the caller's own identity only — `forge login` as the user whose rows you want"
            );
        }
    }
    if token.is_empty() {
        bail!("no bearer configured — `forge login` first; the export reads as your identity");
    }
    Ok(())
}

fn prepare_out_dir(out: &Path) -> Result<()> {
    if out.exists() {
        let mut entries =
            std::fs::read_dir(out).with_context(|| format!("read {}", out.display()))?;
        if entries.next().is_some() {
            bail!(
                "{} is not empty; export into a new directory so two exports never mix",
                out.display()
            );
        }
    }
    std::fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))
}

fn git_state(tree: &Path) -> (Option<String>, bool) {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(tree)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let sha = git(&["rev-parse", "HEAD"]);
    let dirty = sha.is_some() && git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    (sha, dirty)
}

fn http_err(what: &str, e: ForgeError) -> anyhow::Error {
    match e {
        ForgeError::Http { status, body } if status.as_u16() == 401 => anyhow::anyhow!(
            "{what}: 401 — the bearer was rejected; `forge login` again ({})",
            body.trim()
        ),
        other => anyhow::anyhow!("{what}: {other}"),
    }
}

/// UTC now as RFC 3339, second precision (the CLI links no date crate).
fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-from-civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    /// One request the stub saw: its path, its bearer, its read pin.
    #[derive(Debug, Clone)]
    struct Seen {
        path: String,
        auth: Option<String>,
        as_of: Option<String>,
    }

    /// The sequence page one reads at; the stub's write lands after it.
    const SNAPSHOT: u64 = 42;
    const TOKEN: &str = "fr_u_testcaller";

    /// A runtime stand-in. `notes` holds five rows at sequence 42; the first
    /// data read moves the live sequence to 43, at which a sixth row exists
    /// at the FRONT of the scan order, so an unpinned offset walk would answer
    /// a row twice. A pinned read answers the snapshot. `private` answers an
    /// RLS default-deny empty page. `_changelog` must never be read.
    fn stub(pinned_runtime: bool, openapi_forbidden: bool) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let live = Mutex::new(SNAPSHOT);
        let log = seen.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let mut lines = text.split("\r\n");
                let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_string();
                let headers: BTreeMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let req = Seen {
                    path: path.clone(),
                    auth: headers.get("authorization").cloned(),
                    as_of: headers.get(AS_OF).cloned(),
                };
                log.lock().unwrap().push(req.clone());
                let (status, body, extra) = answer(&req, &live, pinned_runtime, openapi_forbidden);
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });
        (base, seen)
    }

    fn answer(
        req: &Seen,
        live: &Mutex<u64>,
        pinned_runtime: bool,
        openapi_forbidden: bool,
    ) -> (&'static str, String, String) {
        if req.path == "/api/v1/platform/openapi" && openapi_forbidden {
            let body = serde_json::json!({
                "error": "PERMISSION_DENIED",
                "message": "missing permission 'platform:read'",
            });
            return ("403 Forbidden", body.to_string(), String::new());
        }
        if req.path == "/api/v1/platform/openapi" {
            let spec = serde_json::json!({ "openapi": "3.1.0", "paths": {
                "/api/v1/data/notes": { "get": {}, "post": {} },
                "/api/v1/data/notes/{id}": { "get": {} },
                "/api/v1/data/private": { "get": {} },
                "/api/v1/data/_changelog": { "get": {} },
                "/api/v1/platform/openapi": { "get": {} },
            }});
            return ("200 OK", spec.to_string(), String::new());
        }
        let (route, query) = req.path.split_once('?').unwrap();
        let param = |k: &str| -> usize {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                .unwrap()
                .parse()
                .unwrap()
        };
        let (limit, offset) = (param("limit"), param("offset"));
        // An unrouted table answers 404 and reads nothing, so it moves no
        // sequence; only a real read stands for the "first read" below.
        if !matches!(
            route,
            "/api/v1/data/notes" | "/api/v1/data/private" | "/api/v1/data/_changelog"
        ) {
            let body = serde_json::json!({ "error": "NOT_FOUND" });
            return ("404 Not Found", body.to_string(), String::new());
        }
        let read_at = match &req.as_of {
            Some(v) => v.parse::<u64>().unwrap(),
            None => {
                let mut l = live.lock().unwrap();
                let at = *l;
                *l = SNAPSHOT + 1; // the write lands after the first read
                at
            }
        };
        let mut extra = String::new();
        let rows: Vec<serde_json::Value> = match route {
            "/api/v1/data/notes" => {
                let mut all: Vec<serde_json::Value> = (0..5)
                    .map(|i| serde_json::json!({ "id": format!("n{i}"), "body": format!("note {i}") }))
                    .collect();
                if read_at > SNAPSHOT {
                    all.insert(
                        0,
                        serde_json::json!({ "id": "late", "body": "written mid-walk" }),
                    );
                }
                all.into_iter().skip(offset).take(limit + 1).collect()
            }
            "/api/v1/data/private" => {
                extra.push_str("x-forge-rls: rls_default_deny\r\n");
                Vec::new()
            }
            "/api/v1/data/_changelog" => panic!("the export read a platform table"),
            _ => {
                let body = serde_json::json!({ "error": "NOT_FOUND" });
                return ("404 Not Found", body.to_string(), String::new());
            }
        };
        let has_more = rows.len() > limit;
        let page: Vec<_> = rows.into_iter().take(limit).collect();
        if pinned_runtime {
            extra.push_str(&format!("{AS_OF}: {read_at}\r\n{HAS_MORE}: {has_more}\r\n"));
        }
        ("200 OK", serde_json::Value::Array(page).to_string(), extra)
    }

    fn client(base: &str, token: &str) -> ForgeClient {
        ForgeClient::new(ResolvedConfig {
            base_url: base.to_string(),
            token: token.to_string(),
            portal: None,
            workspace_id: None,
        })
        .unwrap()
    }

    /// A config tree in git: `drafts` is declared but unrouted, `vault` is
    /// `api.access: hidden`, and `notes` hides a column from the API.
    fn config_tree() -> tempfile::TempDir {
        let tree = tempfile::tempdir().unwrap();
        let schemas = tree.path().join("domains/core/schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let table = |name: &str, api: serde_json::Value| {
            let json = serde_json::json!({
                "name": name,
                "archetype": "Base",
                "columns": [ { "name": "body", "type": "string" } ],
                "api": api,
            });
            std::fs::write(schemas.join(format!("{name}.table.json")), json.to_string()).unwrap();
        };
        table("notes", serde_json::json!({ "hidden": ["body_draft"] }));
        table("drafts", serde_json::json!({}));
        table("vault", serde_json::json!({ "access": "hidden" }));
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(tree.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "tree"]);
        tree
    }

    fn args(out: &Path, tree: &Path) -> ExportArgs {
        ExportArgs {
            out: out.to_path_buf(),
            manifest_dir: Some(tree.to_path_buf()),
            page_size: 2,
        }
    }

    #[tokio::test]
    async fn one_as_of_pins_every_page_and_each_row_is_written_once() {
        let (base, seen) = stub(true, false);
        let tree = config_tree();
        let scratch = tempfile::tempdir().unwrap();
        let out = scratch.path().join("export");
        let manifest = export(&args(&out, tree.path()), &client(&base, TOKEN))
            .await
            .unwrap();

        // Rows: the snapshot's five, each once, and not the mid-walk write.
        let text = std::fs::read_to_string(out.join("notes.jsonl")).unwrap();
        let ids: Vec<String> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["id"].to_string())
            .collect();
        assert_eq!(ids, ["\"n0\"", "\"n1\"", "\"n2\"", "\"n3\"", "\"n4\""]);

        let seen = seen.lock().unwrap().clone();
        // The pin: one unpinned read (page one of the first table), then every
        // page of every table carries the sequence page one answered.
        let data: Vec<&Seen> = seen
            .iter()
            .filter(|s| s.path.starts_with("/api/v1/data/"))
            .collect();
        let notes_pages = data
            .iter()
            .filter(|s| s.path.starts_with("/api/v1/data/notes?"))
            .count();
        assert_eq!(
            notes_pages, 3,
            "five rows at two a page is three pages: {data:?}"
        );
        assert_eq!(
            data[0].as_of, None,
            "page one learns the sequence: {data:?}"
        );
        for s in &data[1..] {
            assert_eq!(s.as_of.as_deref(), Some("42"), "unpinned page: {s:?}");
        }
        // The bearer: the caller's own on every request; never admin/platform.
        for s in &seen {
            assert_eq!(s.auth.as_deref(), Some("Bearer fr_u_testcaller"), "{s:?}");
        }

        assert_eq!(manifest.as_of, SNAPSHOT);
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(on_disk["as_of"], 42);
        assert_eq!(on_disk["format"], "forge-export/1");
        let sha = Command::new("git")
            .arg("-C")
            .arg(tree.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            on_disk["config_tree"]["git_sha"],
            String::from_utf8_lossy(&sha.stdout).trim()
        );
        assert_eq!(on_disk["config_tree"]["dirty"], false);
        let entry = |name: &str| {
            on_disk["tables"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("no manifest entry for {name}: {on_disk}"))
                .clone()
        };
        assert_eq!(entry("notes")["rows"], 5);
        assert_eq!(entry("notes")["exported"], true);
        assert!(
            entry("notes")["note"]
                .as_str()
                .unwrap()
                .contains("body_draft")
        );
        assert_eq!(entry("private")["exported"], true);
        assert_eq!(entry("private")["rows"], 0);
        assert!(
            entry("private")["note"]
                .as_str()
                .unwrap()
                .starts_with("rls-denied")
        );
        assert!(out.join("private.jsonl").exists());
        assert_eq!(entry("_changelog")["exported"], false);
        assert!(
            entry("vault")["note"]
                .as_str()
                .unwrap()
                .starts_with("hidden")
        );
        assert!(
            entry("drafts")["note"]
                .as_str()
                .unwrap()
                .starts_with("unrouted")
        );
        assert!(!out.join("vault.jsonl").exists());
    }

    #[tokio::test]
    async fn an_admin_or_platform_bearer_is_refused_before_any_request() {
        let (base, seen) = stub(true, false);
        let tree = config_tree();
        for token in ["fr_a_operator", "fr_p_sig"] {
            let scratch = tempfile::tempdir().unwrap();
            let out = scratch.path().join("export");
            let err = export(&args(&out, tree.path()), &client(&base, token))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("own identity"), "{err:#}");
        }
        assert!(
            seen.lock().unwrap().is_empty(),
            "a request left: {:?}",
            seen.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn a_runtime_that_answers_no_sequence_is_refused_not_exported_unpinned() {
        let (base, _) = stub(false, false);
        let tree = config_tree();
        let scratch = tempfile::tempdir().unwrap();
        let out = scratch.path().join("export");
        let err = export(&args(&out, tree.path()), &client(&base, TOKEN))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot pin"), "{err:#}");
        assert!(!out.join("manifest.json").exists());
    }

    #[tokio::test]
    async fn an_identity_refused_the_openapi_document_exports_the_config_trees_tables() {
        let (base, seen) = stub(true, true);
        let tree = config_tree();
        let scratch = tempfile::tempdir().unwrap();
        let out = scratch.path().join("export");
        let manifest = export(&args(&out, tree.path()), &client(&base, TOKEN))
            .await
            .unwrap();
        assert!(
            manifest.tables_from.starts_with("config tree"),
            "{}",
            manifest.tables_from
        );
        let lines = std::fs::read_to_string(out.join("notes.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 5);
        let entry = |name: &str| manifest.tables.iter().find(|t| t.name == name).unwrap();
        assert!(entry("notes").exported);
        assert!(!entry("drafts").exported);
        assert!(
            entry("drafts")
                .note
                .as_deref()
                .unwrap()
                .starts_with("unrouted")
        );
        assert!(!out.join("drafts.jsonl").exists());
        assert!(
            entry("vault")
                .note
                .as_deref()
                .unwrap()
                .starts_with("hidden")
        );
        let seen = seen.lock().unwrap();
        assert!(
            !seen
                .iter()
                .any(|s| s.path.starts_with("/api/v1/data/vault")),
            "a hidden table was probed: {seen:?}"
        );
        for s in seen.iter() {
            assert_eq!(s.auth.as_deref(), Some("Bearer fr_u_testcaller"), "{s:?}");
        }
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_721_466), "2026-10-11T12:24:26Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }
}
