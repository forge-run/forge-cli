//! `forge dev` local-render loop (DX plan WS2).
//!
//! Renders a project's pages **locally, with no runtime** — the
//! whole point of the render/runtime split (FORGE_UI_DX_PLAN §2).
//! The build walkers produce component + page records and the scoped
//! CSS bundle; the extracted [`forge_web_render`] engine turns a
//! page into HTML; data bindings are filled from their declared
//! `default` (fixtures). A tiny localhost server serves the result
//! and live-reloads on file change over SSE.
//!
//! Honest fidelity caveats (printed at startup): data is fixtures,
//! not real RLS-shaped rows (`--live` against an ephemeral workspace
//! is the follow-up); host-based surface scoping isn't reproduced;
//! warm-module/deploy semantics and the anon render cache are
//! platform behaviors, not render behaviors, and are out of scope.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use forge_schema::{
    BrandingTokens, ComponentManifest, PageManifest, PropDecl, PropType, PropsSchema,
    SCHEMA_VERSION_CURRENT,
};
use forge_web_render::{ComponentKind, ComponentRecord, ComponentResolver, Scope};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast};

/// The Tier-2 behavior runtime, inlined so locally-served pages get
/// working declarative behaviors without a static-asset pipeline.
const BEHAVIORS_JS: &str = include_str!("../../../forge-web/runtime-js/behaviors.js");
/// Live-view runtime (WS6). Inlined in dev only when a page opts in via
/// a `data-forge-live` root, matching the runtime shell's conditional
/// injection so local render shows the same behavior.
const LIVE_JS: &str = include_str!("../../../forge-web/runtime-js/live.js");

/// Browser snippet that reconnects an EventSource and reloads on any
/// rebuild signal.
const RELOAD_JS: &str = r#"(function(){var s=new EventSource('/__forge_reload');s.onmessage=function(){location.reload()};s.onerror=function(){};})();"#;

// ─── public entry ───────────────────────────────────────────────

pub async fn run_local(
    project_dir: PathBuf,
    port: u16,
    open: bool,
    surface: Option<String>,
) -> Result<()> {
    eprintln!("forge dev — local render (no runtime)");
    eprintln!("  project:  {}", project_dir.display());
    eprintln!(
        "  data:     fixtures — `<page>.page.fixtures.json` if present, else each binding's `default`"
    );
    if let Some(ref s) = surface {
        eprintln!("  surface:  {s} (pages with no declared surface always served)");
    }
    eprintln!();

    let surface = surface; // moved into the loop below
    let site = build_site_safe(&project_dir, surface.as_deref()).unwrap_or_else(|e| {
        eprintln!("initial build error:\n{e:#}");
        Site::error_page(&project_dir, format!("{e:#}"))
    });
    let state = Arc::new(RwLock::new(site));
    let (reload_tx, _) = broadcast::channel::<()>(16);

    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    eprintln!("  serving:  http://{addr}");
    {
        let routes = state.read().await;
        for r in &routes.routes {
            eprintln!("    {}", r.path);
        }
    }
    eprintln!("\nready. edit a file to live-reload. Ctrl-C to exit.\n");

    if open {
        let _ = webbrowser::open(&format!("http://{addr}"));
    }

    // HTTP server task.
    {
        let state = state.clone();
        let reload_tx = reload_tx.clone();
        let project_dir = project_dir.clone();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let mut reload_rx = reload_tx.subscribe();
                        let project_dir = project_dir.clone();
                        tokio::spawn(async move {
                            let _ = handle_conn(stream, state, &mut reload_rx, &project_dir).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("accept error: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });
    }

    // File watcher → rebuild + broadcast reload. notify is sync, so
    // bridge its std channel onto a tokio channel via a blocking
    // thread, then drive rebuilds from the async side.
    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;
    let (raw_tx, raw_rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(std::time::Duration::from_millis(300), None, move |res| {
        let _ = raw_tx.send(res);
    })
    .context("creating file watcher")?;
    for sub in ["pages", "components", "app.json", "service.json", "static", "content"] {
        let p = project_dir.join(sub);
        if p.exists() {
            let _ = debouncer.watch(&p, RecursiveMode::Recursive);
        }
    }
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<()>(16);
    std::thread::spawn(move || {
        for res in raw_rx {
            if res.is_ok() {
                let _ = ev_tx.blocking_send(());
            }
        }
    });

    while ev_rx.recv().await.is_some() {
        match build_site_safe(&project_dir, surface.as_deref()) {
            Ok(site) => {
                *state.write().await = site;
                let _ = reload_tx.send(());
                eprintln!("rebuilt ✓");
            }
            Err(e) => {
                let page = Site::error_page(&project_dir, format!("{e:#}"));
                *state.write().await = page;
                let _ = reload_tx.send(());
                eprintln!("build error (overlay served):\n{e:#}");
            }
        }
    }

    Ok(())
}

// ─── build + render ─────────────────────────────────────────────

struct Site {
    routes: Vec<RouteEntry>,
}

struct RouteEntry {
    path: String,
    segments: Vec<Seg>,
    html: String,
}

enum Seg {
    Lit(String),
    Param,
}

impl Site {
    /// Build a single-route error site whose page is the dev overlay
    /// (WS3). The overlay parses the build error for a component/page
    /// name + line, locates the offending source file, and shows an
    /// excerpt with the line marked — Vite-grade, server-rendered.
    fn error_page(project_dir: &Path, detail: String) -> Site {
        Site {
            routes: vec![RouteEntry {
                path: "/__error".into(),
                segments: vec![],
                html: error_overlay(project_dir, &detail),
            }],
        }
    }
}

/// Render the dev error overlay for a build/render failure.
fn error_overlay(project_dir: &Path, detail: &str) -> String {
    // Pull a surface name + line out of the message. Our build errors
    // read like "component `caller` at line 1 …" / "page `docs` …".
    let name = backtick_after(detail, "component `")
        .or_else(|| backtick_after(detail, "page `"));
    let line = line_number_in(detail);
    let excerpt = name
        .as_deref()
        .and_then(|n| source_excerpt(project_dir, n, line));

    let excerpt_html = match excerpt {
        Some((path, block)) => format!(
            "<div class=\"loc\">{}</div><pre class=\"code\">{}</pre>",
            html_escape(&path),
            block
        ),
        None => String::new(),
    };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>forge dev — error</title><style>\
         :root{{color-scheme:dark}}\
         body{{margin:0;background:#161413;color:#e7e3df;font:14px/1.65 ui-sans-serif,system-ui;padding:48px 40px}}\
         .wrap{{max-width:920px;margin:0 auto}}\
         .tag{{display:inline-block;font:600 11px/1 ui-sans-serif;letter-spacing:.14em;text-transform:uppercase;color:#161413;background:#e8894a;padding:5px 9px;border-radius:5px}}\
         h1{{font:600 22px/1.3 ui-sans-serif;margin:18px 0 4px}}\
         .msg{{white-space:pre-wrap;background:#221715;border-left:3px solid #e8894a;padding:16px 18px;border-radius:8px;font-family:ui-monospace,monospace;font-size:13px;margin:20px 0}}\
         .loc{{font-family:ui-monospace,monospace;font-size:12px;color:#a89f98;margin:18px 0 6px}}\
         .code{{background:#1d1a18;border:1px solid #2c2724;border-radius:8px;padding:14px 0;overflow:auto;font-family:ui-monospace,monospace;font-size:12.5px;line-height:1.7;margin:0}}\
         .code .ln{{display:block;padding:0 18px;white-space:pre}}\
         .code .hot{{background:#3a201a;border-left:2px solid #e8894a;padding-left:16px}}\
         .gut{{color:#5c544e;user-select:none;display:inline-block;width:3ch;text-align:right;margin-right:16px}}\
         .foot{{margin-top:28px;color:#8a817b;font-size:12.5px}}\
         </style></head><body><div class=\"wrap\">\
         <span class=\"tag\">forge dev · build error</span>\
         <h1>This page can't render yet.</h1>\
         <div class=\"msg\">{msg}</div>{excerpt}\
         <div class=\"foot\">Fix the source and save — this overlay reloads automatically.</div>\
         </div><script>{RELOAD_JS}</script></body></html>",
        msg = html_escape(detail),
        excerpt = excerpt_html,
    )
}

/// Extract the first backtick-quoted token following `marker`.
fn backtick_after(s: &str, marker: &str) -> Option<String> {
    let start = s.find(marker)? + marker.len();
    let rest = &s[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Pull a line number out of `… line N …` (first occurrence).
fn line_number_in(s: &str) -> Option<usize> {
    let at = s.find("line ")? + "line ".len();
    let digits: String = s[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Locate `<name>.component.html` or `<name>.page.html` under the
/// project and return (relative-path, marked HTML excerpt around
/// `line`). When no line is known, shows the file head.
fn source_excerpt(project_dir: &Path, name: &str, line: Option<usize>) -> Option<(String, String)> {
    // Templates (where the line number points) take priority over the
    // manifest; search for each target in order so `.html` wins.
    let targets = [
        format!("{name}.component.html"),
        format!("{name}.page.html"),
        format!("{name}.component.json"),
        format!("{name}.page.json"),
    ];
    let all: Vec<std::path::PathBuf> = walkdir::WalkDir::new(project_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    let path = targets.iter().find_map(|t| {
        all.iter()
            .find(|p| p.file_name().and_then(|f| f.to_str()) == Some(t.as_str()))
            .cloned()
    })?;
    let body = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = body.lines().collect();
    let center = line.unwrap_or(1).saturating_sub(1).min(lines.len().saturating_sub(1));
    let from = center.saturating_sub(4);
    let to = (center + 4).min(lines.len().saturating_sub(1));
    let mut block = String::new();
    for (i, text) in lines.iter().enumerate().take(to + 1).skip(from) {
        let n = i + 1;
        let hot = line.map(|l| l == n).unwrap_or(false);
        block.push_str(&format!(
            "<span class=\"ln{}\"><span class=\"gut\">{}</span>{}</span>",
            if hot { " hot" } else { "" },
            n,
            html_escape(text),
        ));
    }
    let rel = path
        .strip_prefix(project_dir)
        .unwrap_or(&path)
        .display()
        .to_string();
    let loc = match line {
        Some(l) => format!("{rel}:{l}"),
        None => rel,
    };
    Some((loc, block))
}

/// Panic-safe wrapper: a panic in the build walkers (malformed input,
/// a `.unwrap()` deep in a dependency) must surface as a red overlay,
/// not take the whole dev server + watcher down. (WS2 gap audit.)
fn build_site_safe(project_dir: &Path, surface: Option<&str>) -> Result<Site> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_site(project_dir, surface)))
    {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!(
            "build panicked (see stderr above) — fix the source and save to retry"
        )),
    }
}

fn build_site(project_dir: &Path, surface: Option<&str>) -> Result<Site> {
    // The walkers write manifests to an env dir; use a throwaway one
    // under the project so we don't touch the real build output.
    let out_env = project_dir.join(".forge-dev/env");
    std::fs::create_dir_all(&out_env).ok();

    let components = forge_web_build::collect_and_bundle_components(project_dir, &out_env)
        .map_err(|e| anyhow::anyhow!("components: {e}"))?;
    let names: std::collections::HashSet<&str> =
        components.iter().map(|r| r.name.as_str()).collect();
    let pages = forge_web_build::collect_and_bundle_pages(project_dir, &out_env, &names)
        .map_err(|e| anyhow::anyhow!("pages: {e}"))?;
    let page_components = forge_web_build::pages_as_component_records(&pages);
    let css = forge_web_build::substrate_css_string(&components, &pages);
    let chrome = read_chrome(project_dir);

    // Build the engine resolver: forge-ui built-ins + app components +
    // pages-as-components. App + page records shadow built-ins.
    let mut map: HashMap<String, ComponentRecord> = HashMap::new();
    for b in forge_ui::BUILTINS {
        if let Some(rec) = engine_record_from_parts(
            b.manifest_json,
            b.template,
            b.css,
            b.behaviors_json,
            false,
        ) {
            map.insert(rec.manifest.name.clone(), rec);
        }
    }
    for c in &components {
        if let Some(rec) = engine_record_from_parts(
            &c.manifest_json,
            &c.template_body,
            &c.css_body,
            &c.behaviors_json,
            c.has_rust,
        ) {
            map.insert(rec.manifest.name.clone(), rec);
        }
    }
    // Pages get a permissive {data,params,auth} schema (their real
    // synthetic manifest is empty, which validate_props would reject).
    for pc in &page_components {
        map.insert(
            pc.name.clone(),
            ComponentRecord {
                manifest: Arc::new(page_manifest(&pc.name)),
                template: Arc::from(pc.template_body.as_str()),
                css: Arc::from(pc.css_body.as_str()),
                behaviors_json: Arc::from(pc.behaviors_json.as_str()),
                kind: ComponentKind::Declarative,
            },
        );
    }
    let resolver: Arc<dyn ComponentResolver> = Arc::new(MapResolver(map));

    // Render each page to a full HTML document.
    let mut routes = Vec::new();
    for page in &pages {
        if page.template_body.is_empty() {
            continue; // Tier-3 Rust-only page — nothing to render locally
        }
        let manifest: PageManifest = match serde_json::from_str(&page.manifest_json) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Surface scoping: skip pages not on the requested surface.
        // Pages with no declared surface are always served.
        if let Some(want) = surface {
            if let Some(list) = &manifest.surface {
                if !list.iter().any(|s| s == want) {
                    continue;
                }
            }
        }
        let title = serde_json::from_str::<Value>(&page.manifest_json)
            .ok()
            .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(str::to_string))
            .unwrap_or_else(|| page.name.clone());

        // Fixture data: each binding's declared `default`, then overlay
        // an optional `<page>.page.fixtures.json` sibling (richer than
        // the thin defaults — fills shapes the template needs, e.g. a
        // nav tree, so data-heavy pages render without placeholders).
        let mut data = Map::new();
        for (binding, decl) in &manifest.data {
            data.insert(binding.clone(), decl.default.clone().unwrap_or(Value::Null));
        }
        let mut params = Map::new();
        let mut auth =
            json!({ "user_id": "", "role": "", "tenant_id": "", "oauth_providers": [] });
        if let Some(fx) = load_fixtures(project_dir, page) {
            apply_fixtures(fx, &mut data, &mut params, &mut auth);
        }
        let props = json!({
            "data": Value::Object(data),
            "params": Value::Object(params),
            "auth": auth,
        });

        let raw = forge_web_render::render_component(
            &resolver,
            &page.name,
            &props,
            Vec::new(),
            Scope::default(),
        )
        .map_err(|e| anyhow::anyhow!("rendering page `{}`: {e}", page.name))?;

        // Mirror the runtime: hoist any `<forge-head>` block into the
        // document <head> (per-page title/meta + the render-blocking
        // theme bootstrap that prevents FOUC), then wrap the body in
        // <forge-page data-page="X"> so page-scoped CSS binds.
        let (body, head_inner) = extract_forge_head(&raw);
        let scoped = format!("<forge-page data-page=\"{name}\">{body}</forge-page>", name = page.name);
        let html = document(&title, &scoped, &css, &chrome, head_inner.as_deref());
        routes.push(RouteEntry {
            path: manifest.route.path.clone(),
            segments: parse_segments(&manifest.route.path),
            html,
        });
    }

    // WS4: component gallery. Render every component against props
    // synthesized from its schema (defaults → enum first value → a
    // type-based placeholder), so the full catalog is browsable AND
    // any render failure is visible (this doubles as render-smoke).
    let mut gallery: Vec<(String, bool)> = Vec::new();
    for c in &components {
        let manifest: ComponentManifest = match serde_json::from_str(&c.manifest_json) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let props = synth_props(&manifest.props);
        let (ok, inner) =
            match forge_web_render::render_component(&resolver, &c.name, &props, Vec::new(), Scope::default()) {
                Ok(h) => (true, h),
                Err(e) => (
                    false,
                    format!("<pre style=\"color:#e8894a;white-space:pre-wrap\">{}</pre>", html_escape(&e.to_string())),
                ),
            };
        let header = format!(
            "<div style=\"padding:14px 20px;border-bottom:1px solid var(--brand-border,#2c2724);font:600 13px ui-sans-serif\">{} {}</div>",
            html_escape(&c.name),
            if ok { "" } else { "· render error" }
        );
        let body = format!("{header}<div style=\"padding:28px\">{inner}</div>");
        routes.push(RouteEntry {
            path: format!("/__gallery/{}", c.name),
            segments: Vec::new(),
            html: document(&format!("{} · gallery", c.name), &body, &css, &chrome, None),
        });
        gallery.push((c.name.clone(), ok));
    }
    let ok_count = gallery.iter().filter(|(_, ok)| *ok).count();
    let mut items = String::new();
    for (name, ok) in &gallery {
        items.push_str(&format!(
            "<li style=\"padding:6px 0\"><a href=\"/__gallery/{n}\" style=\"color:var(--brand-primary,#e8894a);text-decoration:none\">{n}</a> <span style=\"color:{c}\">{s}</span></li>",
            n = html_escape(name),
            c = if *ok { "#7d8a7d" } else { "#e8894a" },
            s = if *ok { "✓" } else { "✗ render error" },
        ));
    }
    let index = format!(
        "<div style=\"padding:40px;max-width:760px;margin:0 auto;font:14px/1.6 ui-sans-serif\">\
         <h1 style=\"font:600 22px ui-sans-serif\">Component gallery</h1>\
         <p style=\"color:var(--brand-foreground-faint,#8a817b)\">{} components · {} render clean</p>\
         <ul style=\"list-style:none;padding:0;columns:2;gap:24px\">{}</ul></div>",
        gallery.len(),
        ok_count,
        items,
    );
    routes.push(RouteEntry {
        path: "/__gallery".into(),
        segments: Vec::new(),
        html: document("Component gallery", &index, &css, &chrome, None),
    });

    routes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Site { routes })
}

/// Synthesize a render-able props object from a component's schema:
/// declared default → first enum value → a type-based placeholder.
/// Used by the gallery so every component renders with content.
fn synth_props(schema: &PropsSchema) -> Value {
    let mut map = Map::new();
    for (name, decl) in &schema.props {
        if let Some(d) = &decl.default {
            map.insert(name.clone(), d.clone());
        } else if let Some(first) = decl.enum_values.as_ref().and_then(|e| e.first()) {
            map.insert(name.clone(), first.clone());
        } else {
            map.insert(name.clone(), synth_by_type(decl.ty, decl.items.as_deref()));
        }
    }
    Value::Object(map)
}

fn synth_by_type(ty: PropType, items: Option<&PropDecl>) -> Value {
    match ty {
        PropType::String => Value::String("Example".into()),
        PropType::Int => Value::from(42),
        PropType::Float => Value::from(3.14),
        PropType::Bool => Value::Bool(true),
        PropType::Array => {
            let item = items
                .map(|i| synth_by_type(i.ty, i.items.as_deref()))
                .unwrap_or(Value::Null);
            Value::Array(vec![item])
        }
        PropType::Object => Value::Object(Map::new()),
    }
}

struct MapResolver(HashMap<String, ComponentRecord>);

impl ComponentResolver for MapResolver {
    fn resolve(&self, name: &str, _scope: Scope) -> Option<ComponentRecord> {
        let bare = name
            .strip_prefix("@app/")
            .or_else(|| name.strip_prefix("@forge/"))
            .unwrap_or(name);
        // @tenant/ components are storage-resident — not available
        // locally; resolve to None so the engine's error boundary
        // renders a placeholder.
        if name.starts_with("@tenant/") {
            return None;
        }
        self.0.get(bare).cloned()
    }
}

fn engine_record_from_parts(
    manifest_json: &str,
    template: &str,
    css: &str,
    behaviors_json: &str,
    has_rust: bool,
) -> Option<ComponentRecord> {
    let manifest: ComponentManifest = serde_json::from_str(manifest_json).ok()?;
    Some(ComponentRecord {
        manifest: Arc::new(manifest),
        template: Arc::from(template),
        css: Arc::from(css),
        behaviors_json: Arc::from(behaviors_json),
        kind: if has_rust {
            ComponentKind::RustGuest
        } else {
            ComponentKind::Declarative
        },
    })
}

/// Permissive component manifest for a page: declares `data`,
/// `params`, `auth` as optional object props so `validate_props`
/// accepts the composed render envelope.
fn page_manifest(name: &str) -> ComponentManifest {
    let mut props = PropsSchema::default();
    for key in ["data", "params", "auth"] {
        props
            .props
            .insert(key.to_string(), PropDecl::scalar(PropType::Object));
    }
    ComponentManifest {
        schema_version: SCHEMA_VERSION_CURRENT.into(),
        name: name.to_string(),
        props,
        slots: Default::default(),
        behaviors: Default::default(),
        description: None,
    }
}

/// Load a page's optional `<name>.page.fixtures.json` sibling (next to
/// its manifest). Returns the parsed JSON when present + valid.
fn load_fixtures(project_dir: &Path, page: &forge_web_build::PageRecord) -> Option<Value> {
    let src = project_dir.join(&page.source);
    let dir = src.parent()?;
    let fx = dir.join(format!("{}.page.fixtures.json", page.name));
    let raw = std::fs::read_to_string(fx).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Overlay fixture JSON onto the default-derived envelope. Two accepted
/// shapes: an envelope `{ "data": {...}, "params": {...}, "auth": {...} }`
/// (each part merged/overridden), or a bare object treated as the `data`
/// map (binding → value), overlaid on the binding defaults.
fn apply_fixtures(fx: Value, data: &mut Map<String, Value>, params: &mut Map<String, Value>, auth: &mut Value) {
    let Some(obj) = fx.as_object() else {
        return;
    };
    let is_envelope =
        obj.contains_key("data") || obj.contains_key("params") || obj.contains_key("auth");
    if is_envelope {
        if let Some(d) = obj.get("data").and_then(|v| v.as_object()) {
            for (k, v) in d {
                data.insert(k.clone(), v.clone());
            }
        }
        if let Some(p) = obj.get("params").and_then(|v| v.as_object()) {
            for (k, v) in p {
                params.insert(k.clone(), v.clone());
            }
        }
        if let Some(a) = obj.get("auth") {
            *auth = a.clone();
        }
    } else {
        for (k, v) in obj {
            data.insert(k.clone(), v.clone());
        }
    }
}

/// App-level chrome read once from `app.json`: the brand `:root`
/// block plus the `shell` block's raw before/after-body + head_extras,
/// so the local document mirrors what the runtime's `wrap_in_shell`
/// produces (`<main id="page-content">`, surrounding chrome). The
/// portal's shell is empty today; this keeps fidelity for apps using it.
struct Chrome {
    brand_css: String,
    before_body: String,
    after_body: String,
    head_extras: String,
}

fn read_chrome(project_dir: &Path) -> Chrome {
    let app = std::fs::read_to_string(project_dir.join("app.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());

    let tokens = app
        .as_ref()
        .and_then(|v| v.get("branding").cloned())
        .and_then(|b| serde_json::from_value::<BrandingTokens>(b).ok())
        .map(|over| BrandingTokens::default().deep_merge_with(&over))
        .unwrap_or_default();
    let vars = tokens
        .to_css_variables()
        .into_iter()
        .map(|(k, v)| format!("{k}: {v};"))
        .collect::<String>();

    let shell = app.as_ref().and_then(|v| v.get("shell"));
    let str_field = |key: &str| {
        shell
            .and_then(|s| s.get(key))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };

    Chrome {
        brand_css: format!(":root{{{vars}}}"),
        before_body: str_field("before_body"),
        after_body: str_field("after_body"),
        head_extras: str_field("head_extras"),
    }
}

fn document(title: &str, body: &str, css: &str, chrome: &Chrome, extra_head: Option<&str>) -> String {
    // The page's `<forge-head>` inner (when present) is injected first
    // so its render-blocking theme bootstrap runs before paint, exactly
    // as the runtime places it. Its `<title>` (if any) precedes ours so
    // the browser uses the page-specific one.
    let head = extra_head.unwrap_or("");
    // If the page's <forge-head> already supplied a <title>, don't emit
    // our default one (the browser would otherwise see two).
    let title_tag = if head.to_ascii_lowercase().contains("<title") {
        String::new()
    } else {
        format!("<title>{}</title>", html_escape(title))
    };
    // Body structure mirrors the runtime's wrap_in_shell:
    // before_body + <main id="page-content">…</main> + after_body.
    format!(
        "<!doctype html><html lang=\"en\"><head>\
         <meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         {head}{title_tag}\
         <style>{brand_css}</style>\
         <style>{css}</style>\
         {head_extras}\
         </head><body>{before}<main id=\"page-content\">{body}</main>{after}\
         <script>{BEHAVIORS_JS}</script>\
         {live_script}\
         <script>{RELOAD_JS}</script>\
         </body></html>",
        brand_css = chrome.brand_css,
        head_extras = chrome.head_extras,
        before = chrome.before_body,
        after = chrome.after_body,
        live_script = if body.contains("data-forge-live") {
            format!("<script>{LIVE_JS}</script>")
        } else {
            String::new()
        },
    )
}

/// Split a `<forge-head>…</forge-head>` block out of rendered page
/// HTML. Returns `(body_without_block, Some(inner))` when present —
/// the inner head tags to hoist into `<head>`. Case-insensitive on the
/// tag name; only the first occurrence is hoisted (a page declares
/// one). Ported from the runtime's `declarative::extract_forge_head`
/// so local render matches production head placement. (WS2 gap audit.)
fn extract_forge_head(html: &str) -> (String, Option<String>) {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<forge-head") else {
        return (html.to_string(), None);
    };
    let Some(gt) = html[open..].find('>') else {
        return (html.to_string(), None);
    };
    let inner_start = open + gt + 1;
    let Some(rel_close) = lower[inner_start..].find("</forge-head>") else {
        return (html.to_string(), None);
    };
    let inner_end = inner_start + rel_close;
    let block_end = inner_end + "</forge-head>".len();
    let inner = html[inner_start..inner_end].to_string();
    let mut body = String::with_capacity(html.len());
    body.push_str(&html[..open]);
    body.push_str(&html[block_end..]);
    (body, Some(inner))
}

// ─── tiny HTTP/1.1 server ───────────────────────────────────────

async fn handle_conn(
    mut stream: tokio::net::TcpStream,
    state: Arc<RwLock<Site>>,
    reload_rx: &mut broadcast::Receiver<()>,
    project_dir: &Path,
) -> Result<()> {
    // Read until the end of the request headers (`\r\n\r\n`) rather
    // than trusting a single `read()` — a request line split across
    // TCP segments would otherwise be parsed truncated and mis-served.
    // Capped so a never-terminating client can't grow the buffer
    // unbounded. (WS2 gap audit.)
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if find_subslice(&buf, b"\r\n\r\n").is_some() || buf.len() > 65_536 {
            break;
        }
    }
    if buf.is_empty() {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf);
    let first = req.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");
    let head_only = method.eq_ignore_ascii_case("HEAD");

    if path == "/__forge_reload" && !head_only {
        return serve_sse(&mut stream, reload_rx).await;
    }

    // Static assets pass through to the project's static dir.
    if let Some(rest) = path.strip_prefix("/static/") {
        return serve_static(&mut stream, project_dir, rest, head_only).await;
    }

    // Match a page route (literal, then param-pattern).
    let html = {
        let site = state.read().await;
        match route_match(&site.routes, path) {
            Some(h) => Some(h.to_string()),
            // When a build error page is the only route, serve it.
            None => site
                .routes
                .iter()
                .find(|r| r.path == "/__error")
                .map(|r| r.html.clone()),
        }
    };
    match html {
        Some(h) => {
            write_response(&mut stream, "200 OK", "text/html; charset=utf-8", h.as_bytes(), head_only).await
        }
        None => {
            let body = not_found_body(&state).await;
            write_response(&mut stream, "404 Not Found", "text/html; charset=utf-8", body.as_bytes(), head_only).await
        }
    }
}

/// First index of `needle` in `hay`.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn route_match<'a>(routes: &'a [RouteEntry], path: &str) -> Option<&'a str> {
    // Exact match first.
    if let Some(r) = routes.iter().find(|r| r.path == path) {
        return Some(&r.html);
    }
    // Param-pattern match (`:x` matches any single segment).
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    for r in routes {
        if r.segments.is_empty() {
            continue;
        }
        if r.segments.len() != parts.len() {
            continue;
        }
        let ok = r.segments.iter().zip(&parts).all(|(seg, got)| match seg {
            Seg::Lit(l) => l == got,
            Seg::Param => !got.is_empty(),
        });
        if ok {
            return Some(&r.html);
        }
    }
    None
}

fn parse_segments(path: &str) -> Vec<Seg> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with(':') {
                Seg::Param
            } else {
                Seg::Lit(s.to_string())
            }
        })
        .collect()
}

async fn not_found_body(state: &Arc<RwLock<Site>>) -> String {
    let site = state.read().await;
    let mut list = String::new();
    for r in &site.routes {
        if r.path == "/__error" {
            continue;
        }
        list.push_str(&format!("<li><a href=\"{p}\">{p}</a></li>", p = html_escape(&r.path)));
    }
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>404 — forge dev</title>\
         <style>body{{font:15px/1.6 system-ui;padding:40px;color:#222}}a{{color:#b8521f}}</style></head>\
         <body><h1>404</h1><p>No page at this path. Known routes:</p><ul>{list}</ul>\
         <script>{RELOAD_JS}</script></body></html>"
    )
}

async fn serve_sse(
    stream: &mut tokio::net::TcpStream,
    reload_rx: &mut broadcast::Receiver<()>,
) -> Result<()> {
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-cache\r\n\
                Connection: keep-alive\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(b": connected\n\n").await?;
    stream.flush().await?;
    // Heartbeat so a client that disconnects while idle is detected
    // (the ping write fails) and the task + its broadcast receiver are
    // reaped, instead of lingering until the next reload. (WS2 audit.)
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(15));
    ping.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            r = reload_rx.recv() => match r {
                Ok(()) => {
                    if stream.write_all(b"data: reload\n\n").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if stream.write_all(b": ping\n\n").await.is_err() {
                    break;
                }
                let _ = stream.flush().await;
            }
        }
    }
    Ok(())
}

async fn serve_static(
    stream: &mut tokio::net::TcpStream,
    project_dir: &Path,
    rest: &str,
    head_only: bool,
) -> Result<()> {
    // Reject traversal AND absolute paths: `Path::join` silently
    // discards the base when the joined component is absolute, so
    // `/static//etc/hosts` (rest = `/etc/hosts`) would escape the
    // static dir. Reject any non-normal component. (WS2 gap audit.)
    use std::path::Component;
    if Path::new(rest)
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
    {
        return write_response(stream, "403 Forbidden", "text/plain", b"forbidden", head_only).await;
    }
    let path = project_dir.join("static").join(rest);
    match tokio::fs::read(&path).await {
        Ok(bytes) => write_response(stream, "200 OK", mime_for(rest), &bytes, head_only).await,
        Err(_) => write_response(stream, "404 Not Found", "text/plain", b"not found", head_only).await,
    }
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    // HEAD: send headers (incl. the real Content-Length) but no body.
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    if !head_only {
        stream.write_all(body).await?;
    }
    stream.flush().await?;
    Ok(())
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" => "application/json",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
