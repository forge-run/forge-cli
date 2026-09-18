//! The consumer tree's pinned units, resolved for `forge check`
//! (shared-code SC-11, D-110).
//!
//! `forge.units.json` (`forge_platform_wire::UnitPins`) names each unit the
//! tree imports by its address — the sha256 of the bytes it must be. This
//! module answers the CLI half of the question the control plane answers at
//! push: which bytes does each pin mean, and where do they go so the lane's
//! own resolution finds them.
//!
//! The bytes come from a LOCAL STORE, `<root>/.forge/units/<address>/<file>`
//! (or `$FORGE_UNITS_STORE`), which `forge units pull` fills from the
//! owner's published records and which is verified here by digest: a file
//! whose sha256 is not its directory's name is refused, so nothing a
//! customer edits into the store is ever checked as the pinned unit. The
//! store is never the tree — a pinned unit is never written beside the
//! customer's sources — so the check stages a copy of each domain's
//! `services/` into scratch, puts the pins beside the consumers there
//! exactly as the control plane does (`partition::stage_pins`), and reports
//! every path back under the workspace root.

use std::path::{Path, PathBuf};

use forge_lang_rustgen::partition::{self, Pin, snake_case};
use forge_platform_wire::{UNIT_PINS_PATH, UnitAddress, UnitPins};
use sha2::{Digest, Sha256};

/// Where the pulled units live. `$FORGE_UNITS_STORE`, else the workspace's
/// own `.forge/units`.
pub fn store_dir(root: &Path) -> PathBuf {
    std::env::var_os("FORGE_UNITS_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".forge").join("units"))
}

/// The address of these bytes, held to the wire crate's test vector by
/// `address_matches_the_wire_test_vector` below — the same derivation the
/// control plane carries in `admin_units::unit_address`.
pub fn unit_address(source: &[u8]) -> UnitAddress {
    let digest: [u8; 32] = Sha256::digest(source).into();
    UnitAddress::from_sha256(&digest)
}

/// The pin file, if the tree carries one.
pub fn read(root: &Path) -> Result<Option<UnitPins>, String> {
    let path = root.join(UNIT_PINS_PATH);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(None);
    };
    UnitPins::parse(&bytes).map(Some).map_err(|e| e.to_string())
}

/// Resolve every pin from the store, verifying each by digest.
pub fn resolve(root: &Path, pins: &UnitPins) -> Result<Vec<Pin>, String> {
    let store = store_dir(root);
    let mut out = Vec::with_capacity(pins.units.len());
    for (name, address) in &pins.units {
        let dir = store.join(address.as_str());
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        let file = match files.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(format!(
                    "shared unit `{name}` is pinned at {address}, which is not in the local unit \
                     store ({}); `forge units pull` fetches it from the workspace's owner",
                    store.display()
                ));
            }
            many => {
                return Err(format!(
                    "the unit store holds {} files under {address}; one address is one file",
                    many.len()
                ));
            }
        };
        let source = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let actual = unit_address(&source);
        if &actual != address {
            return Err(format!(
                "{} does not hash to its address: it is {actual}, the pin names {address}; the \
                 store was edited — `forge units pull` restores it",
                file.display()
            ));
        }
        let file_name = file
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = file_name
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(&file_name);
        if snake_case(stem) != *name {
            return Err(format!(
                "shared unit `{name}` is pinned at {address}, but those bytes were published as \
                 `{file_name}`, whose module identity is `{}`; the pin's name is what the import \
                 spells",
                snake_case(stem)
            ));
        }
        out.push(Pin {
            file_name,
            address: address.to_string(),
            source,
        });
    }
    Ok(out)
}

/// A scratch copy of the tree's dialect surface with the pins staged beside
/// their consumers. Dropping it removes the copy.
pub struct Staged {
    _dir: tempfile::TempDir,
    /// The copy's root, laid out as the workspace is: `domains/<d>/services`.
    pub root: PathBuf,
}

/// Copy every domain's `services/` (files and the Java unit directories)
/// into scratch, stage the pins into each, and prune the ones no source of
/// that domain reaches — the control plane's `splice_pins` plus
/// `prune_pins`, over a directory instead of a tree map.
pub fn stage(root: &Path, pins: &[Pin]) -> Result<Staged, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("scratch for the pinned check: {e}"))?;
    let copy = dir.path().to_path_buf();
    if root.join(".forge-interpret").is_file() {
        std::fs::write(copy.join(".forge-interpret"), b"").map_err(|e| e.to_string())?;
    }
    for domain in crate::dialect::domains(root) {
        let Some(name) = domain.file_name() else {
            continue;
        };
        let from = domain.join("services");
        if !from.is_dir() {
            continue;
        }
        let to = copy.join("domains").join(name).join("services");
        copy_dir(&from, &to)?;
        let staged = partition::stage_pins(&to, pins)?;
        let mut all = crate::dialect::sources_of(&copy.join("domains").join(name));
        all.extend(staged.iter().cloned());
        let split = partition::partition(&all);
        partition::prune_unreached(&staged, &split.units);
    }
    Ok(Staged {
        _dir: dir,
        root: copy,
    })
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// A path under the scratch copy, spelled under the workspace root again,
/// so a verdict names the customer's file and not a temporary one.
pub fn unstage(text: &str, staged: &Path, root: &Path) -> String {
    text.replace(&staged.display().to_string(), &root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_matches_the_wire_test_vector() {
        for (bytes, expected) in UnitAddress::TEST_VECTOR {
            assert_eq!(unit_address(bytes).as_str(), expected);
        }
    }
}
