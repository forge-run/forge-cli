//! IP-3 (D-106): the customer CLI hands over no toolchain. `forge build` is
//! not a verb, and `forge --help` lists none of the wasm verbs: `wasm-build`,
//! `wasm-upload` and `ship` stay callable, hidden, for `forge dev` and the
//! portal's ship, but a customer reading the help never meets them.

use std::process::Command;

fn forge(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(args)
        .env("HOME", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .expect("spawning the forge binary")
}

#[test]
fn forge_build_is_an_unknown_subcommand() {
    let out = forge(&["build"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "`forge build` succeeded: {stderr}");
    assert!(
        stderr.contains("unrecognized subcommand 'build'"),
        "`forge build` must be refused as an unknown subcommand; stderr:\n{stderr}"
    );
}

#[test]
fn help_lists_no_build_verb() {
    let out = forge(&["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    // The first word of each line under `Commands:` is a listed subcommand.
    let listed: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(listed.contains(&"push"), "help:\n{help}");
    for verb in ["wasm-build", "wasm-upload", "ship", "build"] {
        assert!(
            !listed.contains(&verb),
            "`forge --help` lists `{verb}`:\n{help}"
        );
    }
}

#[test]
fn the_hidden_wasm_verbs_stay_callable() {
    for verb in ["wasm-build", "wasm-upload", "ship"] {
        let out = forge(&[verb, "--help"]);
        assert!(
            out.status.success(),
            "`forge {verb} --help` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
