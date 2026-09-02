//! P10.1: the checker and the compiler are a SINGLE library crate consumed
//! by both the CLI and the build service — never two implementations
//! agreeing by convention (FUTURE.md § server-side compilation). This pins
//! the CLI half: `forge check`/`forge build` link the same
//! `forge-lang-rustgen` the control plane's serving check links, as a path
//! dep into the flat sibling checkout. The server half of the pin is the
//! sibling test in forge-control-plane; deploy-time binary skew is handled
//! at converge time by the release-skew stamp (emitter_version in
//! forge.lock).

#[test]
fn forge_lang_deps_are_path_deps_into_the_sibling_checkout() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let mut seen = Vec::new();
    for line in manifest.lines() {
        let t = line.trim();
        if !t.starts_with("forge-lang") || !t.contains('=') {
            continue;
        }
        assert!(
            t.contains("path = \"../forge-lang/crates/"),
            "a forge-lang dependency that is not a sibling path dep: {t}"
        );
        assert!(
            !t.contains("git =") && !t.contains("version ="),
            "a forge-lang dependency pinned outside the sibling checkout: {t}"
        );
        seen.push(t.split('=').next().unwrap().trim().to_string());
    }
    assert!(
        seen.iter().any(|s| s == "forge-lang-rustgen"),
        "expected forge-lang-rustgen among the forge-lang deps, saw {seen:?}"
    );
}
