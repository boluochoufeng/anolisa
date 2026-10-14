//! Desktop sideload CLI contracts; macOS exercises mutations, Linux verifies rejection.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use anolisa_platform::fs_layout::FsLayout;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod common;

struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    layout: FsLayout,
    app: PathBuf,
    desktop: PathBuf,
    bundle: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let layout = FsLayout::user_with_overrides(
            home.clone(),
            Some(root.join("data")),
            Some(root.join("config")),
            Some(root.join("state")),
            Some(root.join("cache")),
            Some(root.join("runtime")),
        );
        let bundle = layout.datadir.join("adapters/tokenless/qwenwork");
        let files = [
            (
                ".qoder-plugin/plugin.json",
                r#"{"name":"tokenless","version":"1.0.0"}"#,
            ),
            ("hooks/hooks.json", "{}"),
            ("hooks/run-hook.sh", "# fixture"),
            ("hooks/shared/rewrite_hook.py", "# fixture"),
            ("hooks/shared/compress_response_hook.py", "# fixture"),
            ("hooks/shared/hook_utils.py", "# fixture"),
            ("hooks/shared/tool_categories.json", "{}"),
            ("scripts/lifecycle.py", "# fixture"),
            ("scripts/detect.sh", "# fixture"),
            ("scripts/install.sh", "# fixture"),
            ("scripts/uninstall.sh", "# fixture"),
        ];
        let mut inventory = String::new();
        for (relative, contents) in files {
            let path = bundle.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            inventory.push_str(&format!("\n[[objects.files]]\npath = {:?}\nowner = \"anolisa\"\nkind = \"file\"\nsha256 = \"{:x}\"\n", path, Sha256::digest(contents)));
        }
        let manifest = layout.snapshot_path("tokenless");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(
            manifest,
            include_str!("../../../../../src/tokenless/.anolisa/component.toml.in")
                .replace("@VERSION@", "1.0.0"),
        )
        .unwrap();
        fs::create_dir_all(&layout.state_dir).unwrap();
        fs::write(
            layout.state_dir.join("installed.toml"),
            format!(
                r#"schema_version = 2
updated_at = "2026-09-18T00:00:00Z"
install_mode = "user"
prefix = {:?}
anolisa_version = "0.3.17"
[[objects]]
kind = "component"
name = "tokenless"
version = "1.0.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
installed_at = "2026-09-18T00:00:00Z"
{}
"#,
                layout.prefix, inventory
            ),
        )
        .unwrap();
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        let tokenless = home.join(".local/bin/tokenless");
        fs::write(&tokenless, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(tokenless, fs::Permissions::from_mode(0o755)).unwrap();
        let app = home.join("Applications/QwenWorkCN.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(app.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>cn.qwenwork.desktop.mac</string><key>CFBundleShortVersionString</key><string>1.2.5</string></dict></plist>"#).unwrap();
        Self {
            _tmp: tmp,
            desktop: home.join("desktop data"),
            home,
            layout,
            app,
            bundle,
        }
    }

    fn run(&self, flags: &[&str], command: &[&str]) -> Output {
        let mut args = vec!["--install-mode", "user"];
        args.extend_from_slice(flags);
        args.push("adapter");
        args.extend_from_slice(command);
        let root = self.home.parent().unwrap();
        common::run_with_path_env(
            &args,
            &[
                ("HOME", &self.home),
                ("XDG_DATA_HOME", &root.join("data")),
                ("XDG_CONFIG_HOME", &root.join("config")),
                ("XDG_STATE_HOME", &root.join("state")),
                ("XDG_CACHE_HOME", &root.join("cache")),
                ("XDG_RUNTIME_DIR", &root.join("runtime")),
                ("TOKENLESS_QWENWORK_APP", &self.app),
                ("TOKENLESS_QWENWORK_HOME", &self.desktop),
            ],
        )
    }
}

fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn qwenwork_cli_platform_and_lifecycle() {
    let world = World::new();
    let scan = json(world.run(&["--json"], &["scan"]));
    let row = scan["data"]["adapters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["framework"] == "qwenwork")
        .unwrap();
    assert_eq!(row["driver_available"], true);
    assert_eq!(row["resource_present"], true);
    let original = fs::read(world.layout.state_dir.join("installed.toml")).unwrap();
    let preview = world.run(
        &["--json", "--dry-run"],
        &["enable", "tokenless", "qwenwork"],
    );
    assert!(!world.desktop.exists());
    assert_eq!(
        fs::read(world.layout.state_dir.join("installed.toml")).unwrap(),
        original
    );
    if !cfg!(target_os = "macos") {
        assert_eq!(row["framework_detected"], false);
        assert!(!preview.status.success());
        assert!(String::from_utf8_lossy(&preview.stdout).contains("macOS local mode"));
        let enabled = world.run(&["--json"], &["enable", "tokenless", "qwenwork"]);
        assert!(!enabled.status.success());
        assert!(!world.desktop.exists());
        assert_eq!(
            fs::read(world.layout.state_dir.join("installed.toml")).unwrap(),
            original
        );
        return;
    }
    assert_eq!(row["framework_detected"], true);
    json(preview);
    let enabled = json(world.run(&["--json"], &["enable", "tokenless", "qwenwork"]));
    assert!(
        enabled["data"]["notices"]
            .to_string()
            .contains("not verified")
    );
    let target = world.desktop.join("plugins-custom/tokenless");
    assert!(target.join(".tokenless-qwenwork-owned.json").is_file());
    let text = world.run(&[], &["enable", "tokenless", "qwenwork"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("Sideloaded tokenless/qwenwork"));
    let status = json(world.run(&["--json"], &["status", "tokenless"]));
    assert_eq!(status["data"]["receipts"][0]["summary"], "unknown");
    assert!(
        status["data"]["receipts"][0]["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "activation_enabled" && c["status"] == "unknown")
    );
    fs::write(target.join("hooks/run-hook.sh"), "user change").unwrap();
    assert!(
        !world
            .run(&["--json"], &["disable", "tokenless", "qwenwork"])
            .status
            .success()
    );
    assert!(target.exists());
    fs::write(target.join("hooks/run-hook.sh"), "# fixture").unwrap();
    fs::remove_dir_all(&world.bundle).unwrap();
    fs::remove_dir_all(&world.app).unwrap();
    json(world.run(
        &["--json", "--dry-run"],
        &["disable", "tokenless", "qwenwork"],
    ));
    assert!(target.exists());
    json(world.run(&["--json"], &["disable", "tokenless", "qwenwork"]));
    assert!(!target.exists());
    assert!(Path::new(&world.desktop).is_dir());
}
