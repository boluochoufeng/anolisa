//! Ownership-compatible QwenWork inventories and Manager-controlled tree replacement.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::AdapterError;
use super::super::link_transaction::rename;

const MARKER: &str = ".tokenless-qwenwork-owned.json";

// Match the standalone Tokenless installer's JSON contract, including directory
// permissions and the root entry ".". Unknown fields must not widen ownership.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum Entry {
    Directory { mode: u32 },
    File { mode: u32, sha256: String },
}

type Inventory = BTreeMap<String, Entry>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    owner: String,
    schema: u32,
    entries: Inventory,
}

fn fail(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn ignored(name: &str) -> bool {
    name == MARKER || name == "__pycache__" || name.ends_with(".pyc") || name.ends_with(".in")
}

fn inventory(root: &Path, source: bool) -> io::Result<Inventory> {
    fn visit(root: &Path, path: &Path, source: bool, entries: &mut Inventory) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        let relative = path.strip_prefix(root).map_err(io::Error::other)?;
        let name = if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative
                .to_str()
                .ok_or_else(|| fail("non-UTF-8 plugin path"))?
                .to_string()
        };
        let mode = metadata.mode() & 0o7777;
        if metadata.is_dir() {
            entries.insert(name, Entry::Directory { mode });
            for child in fs::read_dir(path)? {
                let child = child?;
                let child_name = child.file_name();
                let child_name = child_name
                    .to_str()
                    .ok_or_else(|| fail("non-UTF-8 plugin path"))?;
                if (path == root && child_name == MARKER) || (source && ignored(child_name)) {
                    continue;
                }
                visit(root, &child.path(), source, entries)?;
            }
        } else if metadata.is_file() && metadata.nlink() == 1 {
            entries.insert(
                name,
                Entry::File {
                    mode,
                    sha256: format!("{:x}", Sha256::digest(fs::read(path)?)),
                },
            );
        } else {
            return Err(fail(format!(
                "refusing symlink, hard link or special file: {}",
                path.display()
            )));
        }
        Ok(())
    }
    let mut entries = Inventory::new();
    visit(root, root, source, &mut entries)?;
    Ok(entries)
}

fn digest(entries: &Inventory) -> io::Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(entries)?)
    ))
}

/// Hash the exact copy inventory before the Manager persists mutation authority.
pub(super) fn source_digest(root: &Path) -> io::Result<String> {
    digest(&inventory(root, true)?)
}

/// Refuse to copy extra or altered files outside the package inventory.
pub(super) fn validate_source_revision(
    revision: &super::super::claim::AdapterSourceRevision,
) -> io::Result<()> {
    for (relative, entry) in inventory(&revision.source_root, true)? {
        if let Entry::File { sha256, .. } = entry {
            let expected = revision
                .files
                .iter()
                .find(|file| file.relative_path == Path::new(&relative));
            if !expected.is_some_and(|file| {
                file.kind == super::super::claim::ManagedFileKind::File
                    && file.sha256.as_deref() == Some(&sha256)
            }) {
                return Err(fail(format!(
                    "QwenWork source is not a matching package-owned file: {relative}"
                )));
            }
        }
    }
    Ok(())
}

/// Check all existing managed ancestors without following a directory alias.
pub(super) fn check_path(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if !meta.is_dir() => {
                return Err(fail(format!(
                    "refusing non-directory or symlink in plugin path: {}",
                    ancestor.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Verify the standalone installer marker against every live entry.
pub(super) fn owned_digest(root: &Path) -> io::Result<Option<String>> {
    check_path(root)?;
    if !root.try_exists()? {
        return Ok(None);
    }
    let marker = read_marker(root)?;
    let actual = inventory(root, false)?;
    if marker.entries != actual {
        return Err(fail(format!(
            "plugin modified; refusing overwrite or removal: {}",
            root.display()
        )));
    }
    digest(&actual).map(Some)
}

fn read_marker(root: &Path) -> io::Result<Marker> {
    let path = root.join(MARKER);
    let metadata = fs::symlink_metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "foreign plugin or missing ownership marker {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(fail(format!("unsafe ownership marker: {}", path.display())));
    }
    let marker: Marker = serde_json::from_slice(&fs::read(&path)?)?;
    if marker.owner != "tokenless/qwenwork" || marker.schema != 1 {
        return Err(fail(format!(
            "foreign ownership marker: {}",
            path.display()
        )));
    }
    Ok(marker)
}

// Cleanup retains the marker until all recorded entries have been removed.
// A subset is valid only in the non-scanned recovery slot and only against a
// full inventory hash already persisted in the receipt.
/// Recognize intact or partially removed receipt-owned recovery trees.
pub(super) fn pending_complete(root: &Path, digests: &[String]) -> io::Result<Option<bool>> {
    check_path(root)?;
    if !root.try_exists()? {
        return Ok(None);
    }
    if fs::read_dir(root)?.next().is_none() {
        return Ok(Some(false));
    }
    let marker = read_marker(root)?;
    if !digests.contains(&digest(&marker.entries)?) {
        return Err(fail(format!(
            "pending inventory differs from receipt: {}",
            root.display()
        )));
    }
    let actual = inventory(root, false)?;
    if actual
        .iter()
        .any(|(path, entry)| marker.entries.get(path) != Some(entry))
    {
        return Err(fail(format!(
            "pending plugin modified; preserving {}",
            root.display()
        )));
    }
    Ok(Some(actual == marker.entries))
}

fn remove_pending(root: &Path, digests: &[String]) -> io::Result<()> {
    if pending_complete(root, digests)?.is_none() {
        return Ok(());
    }
    if fs::read_dir(root)?.next().is_none() {
        return fs::remove_dir(root);
    }
    let entries = inventory(root, false)?;
    for (relative, entry) in entries.iter().rev() {
        if relative == "." {
            continue;
        }
        let path = root.join(relative);
        match entry {
            Entry::Directory { .. } => fs::remove_dir(path)?,
            Entry::File { .. } => fs::remove_file(path)?,
        }
    }
    fs::remove_file(root.join(MARKER))?;
    fs::remove_dir(root)
}

/// Require both standalone ownership and the previously recorded inventory.
pub(super) fn authorized(root: &Path, digests: &[String]) -> io::Result<Option<String>> {
    let actual = owned_digest(root)?;
    if actual
        .as_ref()
        .is_some_and(|value| !digests.contains(value))
    {
        return Err(fail(format!(
            "plugin inventory differs from receipt: {}",
            root.display()
        )));
    }
    Ok(actual)
}

fn sync(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn copy(source: &Path, target: &Path, entries: &Inventory) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for (relative, entry) in entries {
        let destination = target.join(relative);
        match entry {
            Entry::Directory { .. } => fs::create_dir_all(&destination)?,
            Entry::File { mode, .. } => {
                // Recheck the source at copy time; packaged bundles must already
                // contain self-contained hooks, never unresolved source links.
                let source_file = source.join(relative);
                let metadata = fs::symlink_metadata(&source_file)?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    return Err(fail(format!("source changed: {}", source_file.display())));
                }
                fs::copy(source_file, &destination)?;
                fs::set_permissions(&destination, fs::Permissions::from_mode(*mode))?;
                sync(&destination)?;
            }
        }
    }
    for (relative, entry) in entries.iter().rev() {
        if let Entry::Directory { mode } = entry {
            fs::set_permissions(target.join(relative), fs::Permissions::from_mode(*mode))?;
        }
    }
    if inventory(target, false)? != *entries {
        return Err(fail("source changed while staging QwenWork plugin"));
    }
    fs::write(
        target.join(MARKER),
        serde_json::to_vec_pretty(&Marker {
            owner: "tokenless/qwenwork".to_string(),
            schema: 1,
            entries: entries.clone(),
        })?,
    )?;
    fs::set_permissions(target.join(MARKER), fs::Permissions::from_mode(0o600))?;
    sync(&target.join(MARKER))?;
    sync(target)
}

// The pending slot is outside plugins-custom. A sealed tree in either slot is
// safe to recover only if its inventory was persisted in the receipt first.
/// Apply or remove a sideload using only the Manager-validated transaction slots.
pub(crate) fn reconcile(
    target: &Path,
    pending: &Path,
    digests: &[String],
    replacement: Option<&Path>,
) -> Result<(), AdapterError> {
    reconcile_inner(target, pending, digests, replacement, || Ok(()))
        .map_err(|source| AdapterError::Io {
            path: target.to_path_buf(),
            source: fail(format!("{source}; preserved QwenWork slots {} and {}; restore conflicting files and retry with the same data directory", target.display(), pending.display())),
        })
}

fn reconcile_inner(
    target: &Path,
    pending: &Path,
    digests: &[String],
    replacement: Option<&Path>,
    after_publish: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let data = pending
        .parent()
        .ok_or_else(|| fail("missing QwenWork data directory"))?;
    if target != data.join("plugins-custom/tokenless")
        || pending != data.join(".anolisa-qwenwork-tokenless")
    {
        return Err(fail("invalid QwenWork transaction slots"));
    }
    let current = authorized(target, digests)?;
    let retained = pending_complete(pending, digests)?;
    if retained.is_some() {
        if current.is_none() && replacement.is_some() && retained == Some(true) {
            fs::create_dir_all(data.join("plugins-custom"))?;
            rename(pending, target, false)?;
        } else {
            remove_pending(pending, digests)?;
        }
        sync(data)?;
    }
    let Some(source) = replacement else {
        if authorized(target, digests)?.is_some() {
            rename(target, pending, false)?;
            sync(data)?;
            sync(&data.join("plugins-custom"))?;
            after_publish()?;
            authorized(pending, digests)?;
            remove_pending(pending, digests)?;
            sync(data)?;
        }
        return Ok(());
    };
    let entries = inventory(source, true)?;
    let next_digest = digest(&entries)?;
    if !digests.contains(&next_digest) {
        return Err(fail("QwenWork source changed after receipt preparation"));
    }
    if authorized(target, digests)?.as_ref() == Some(&next_digest) {
        return Ok(());
    }
    fs::create_dir_all(data.join("plugins-custom"))?;
    check_path(target)?;
    let staging = tempfile::Builder::new()
        .prefix(".anolisa-qwenwork-stage-")
        .tempdir_in(data)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let staged = staging.path().join("plugin");
    copy(source, &staged, &entries)?;
    let previous = authorized(target, digests)?;
    rename(&staged, pending, false)?;
    sync(data)?;
    // No-replace for initial install, atomic exchange for upgrades. An
    // unsupported filesystem fails before moving the working installation.
    rename(pending, target, previous.is_some())?;
    sync(data)?;
    sync(&data.join("plugins-custom"))?;
    after_publish()?;
    if previous.is_some() {
        if authorized(pending, digests)? != previous {
            return Err(fail("displaced plugin changed during replacement"));
        }
        remove_pending(pending, digests)?;
        sync(data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn setup() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("hooks")).unwrap();
        fs::write(source.join("hooks/run.sh"), "old").unwrap();
        fs::set_permissions(
            source.join("hooks/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let target = root.join("data/plugins-custom/tokenless");
        let pending = root.join("data/.anolisa-qwenwork-tokenless");
        (tmp, source, target, pending)
    }

    #[test]
    fn install_upgrade_idempotence_and_source_independent_removal() {
        let (_tmp, source, target, pending) = setup();
        let mut digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert_eq!(owned_digest(&target).unwrap(), Some(digests[0].clone()));
        let inode = fs::metadata(&target).unwrap().ino();
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
        fs::write(source.join("hooks/run.sh"), "new").unwrap();
        digests.push(source_digest(&source).unwrap());
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("hooks/run.sh")).unwrap(),
            "new"
        );
        assert!(!pending.exists());
        fs::remove_dir_all(&source).unwrap();
        reconcile(&target, &pending, &digests, None).unwrap();
        reconcile(&target, &pending, &digests, None).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn interrupted_upgrade_and_disable_are_retryable() {
        let (_tmp, source, target, pending) = setup();
        let mut digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::write(source.join("hooks/run.sh"), "next").unwrap();
        digests.push(source_digest(&source).unwrap());
        reconcile_inner(&target, &pending, &digests, Some(&source), || {
            Err(fail("simulated process interruption"))
        })
        .unwrap_err();
        assert_eq!(
            fs::read_to_string(target.join("hooks/run.sh")).unwrap(),
            "next"
        );
        assert_eq!(
            fs::read_to_string(pending.join("hooks/run.sh")).unwrap(),
            "old"
        );
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert!(!pending.exists());
        reconcile_inner(&target, &pending, &digests, None, || {
            Err(fail("interrupted removal"))
        })
        .unwrap_err();
        assert!(!target.exists());
        assert!(pending.exists());
        reconcile(&target, &pending, &digests, None).unwrap();
        assert!(!pending.exists());
    }

    #[test]
    fn interrupted_unpublish_restores_before_reenable() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::rename(&target, &pending).unwrap();
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert!(target.exists());
        assert!(!pending.exists());
    }

    #[test]
    fn changed_or_foreign_files_are_preserved() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("user.txt"), "keep").unwrap();
        assert!(reconcile(&target, &pending, &digests, Some(&source)).is_err());
        assert_eq!(fs::read_to_string(target.join("user.txt")).unwrap(), "keep");
        fs::remove_dir_all(&target).unwrap();
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::write(target.join("hooks/run.sh"), "user modification").unwrap();
        assert!(reconcile(&target, &pending, &digests, Some(&source)).is_err());
        assert!(reconcile(&target, &pending, &digests, None).is_err());
        assert_eq!(
            fs::read_to_string(target.join("hooks/run.sh")).unwrap(),
            "user modification"
        );
    }

    #[test]
    fn links_permissions_and_marker_tampering_are_detected() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::set_permissions(
            target.join("hooks/run.sh"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(owned_digest(&target).is_err());
        fs::set_permissions(
            target.join("hooks/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink(&source, target.join("escape")).unwrap();
        assert!(owned_digest(&target).is_err());
        fs::remove_file(target.join("escape")).unwrap();
        fs::hard_link(target.join("hooks/run.sh"), target.join("hardlink")).unwrap();
        assert!(owned_digest(&target).is_err());
        fs::remove_file(target.join("hardlink")).unwrap();
        fs::write(target.join("hooks/run.sh"), "forged").unwrap();
        let forged = Marker {
            owner: "tokenless/qwenwork".into(),
            schema: 1,
            entries: inventory(&target, false).unwrap(),
        };
        fs::write(target.join(MARKER), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(owned_digest(&target).is_ok());
        assert!(authorized(&target, &digests).is_err());
    }

    #[test]
    fn partial_cleanup_retries_without_restoring_deleted_files() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::rename(&target, &pending).unwrap();
        fs::remove_file(pending.join("hooks/run.sh")).unwrap();
        assert_eq!(pending_complete(&pending, &digests).unwrap(), Some(false));
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        assert!(target.join("hooks/run.sh").exists());
        fs::rename(&target, &pending).unwrap();
        fs::remove_file(pending.join("hooks/run.sh")).unwrap();
        reconcile(&target, &pending, &digests, None).unwrap();
        assert!(!pending.exists());
        // Interruption after the marker is removed leaves only an empty slot.
        fs::create_dir(&pending).unwrap();
        reconcile(&target, &pending, &digests, None).unwrap();
        assert!(!pending.exists());
    }

    #[test]
    fn copy_requires_matching_package_inventory() {
        use crate::adapter::claim::{AdapterSourceRevision, ManagedFileKind, ManagedSourceFile};
        let (_tmp, source, _, _) = setup();
        let revision = AdapterSourceRevision {
            source_root: source.clone(),
            materialized_sources: Vec::new(),
            files: vec![ManagedSourceFile {
                relative_path: "hooks/run.sh".into(),
                kind: ManagedFileKind::File,
                sha256: Some(format!("{:x}", Sha256::digest(b"old"))),
                symlink_target: None,
            }],
        };
        validate_source_revision(&revision).unwrap();
        fs::write(source.join("unmanaged.sh"), "unmanaged").unwrap();
        assert!(validate_source_revision(&revision).is_err());
        fs::remove_file(source.join("unmanaged.sh")).unwrap();
        fs::write(source.join("hooks/run.sh"), "changed").unwrap();
        assert!(validate_source_revision(&revision).is_err());
    }

    #[test]
    fn source_change_and_pending_conflict_leave_working_installation() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::write(source.join("hooks/run.sh"), "changed after preparation").unwrap();
        assert!(reconcile(&target, &pending, &digests, Some(&source)).is_err());
        assert_eq!(
            fs::read_to_string(target.join("hooks/run.sh")).unwrap(),
            "old"
        );
        fs::create_dir(&pending).unwrap();
        fs::write(pending.join("keep"), "foreign").unwrap();
        assert!(reconcile(&target, &pending, &digests, None).is_err());
        assert!(target.exists());
        assert!(pending.join("keep").exists());
    }

    #[test]
    fn standalone_python_marker_is_bidirectionally_compatible() {
        let (_tmp, source, target, pending) = setup();
        let digests = vec![source_digest(&source).unwrap()];
        let installer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../src/tokenless/adapters/tokenless/qwenwork/scripts/lifecycle.py");
        let code = "import runpy,sys,json,pathlib,shutil; m=runpy.run_path(sys.argv[1]); t=pathlib.Path(sys.argv[3]); shutil.copytree(sys.argv[2],t); (t/m['MARKER']).write_text(json.dumps({'owner':m['OWNER'],'schema':1,'entries':m['_inventory'](t)})); m['_owned'](t)";
        let result = std::process::Command::new("python3")
            .args(["-B", "-c", code])
            .arg(&installer)
            .arg(&source)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        fs::write(source.join("hooks/run.sh"), "Rust upgrade").unwrap();
        let mut digests = digests;
        digests.push(source_digest(&source).unwrap());
        reconcile(&target, &pending, &digests, Some(&source)).unwrap();
        let result = std::process::Command::new("python3").args(["-B", "-c", "import runpy,sys,pathlib; runpy.run_path(sys.argv[1])['_owned'](pathlib.Path(sys.argv[2]))"]).arg(&installer).arg(&target).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
