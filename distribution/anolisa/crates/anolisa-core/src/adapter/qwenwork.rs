//! QwenWorkCN desktop sideloads for macOS local sessions.
//!
//! Filesystem ownership is verifiable; activation and loaded hooks require the
//! desktop UI. No Qoder CLI, settings database, or packaged lifecycle script is used.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::manifest::{AdapterNotice, NoticeLevel, NoticeWhen};

use super::AdapterError;
use super::claim::{
    AdapterClaim, CLAIM_SCHEMA_VERSION, ClaimResource, ClaimResourceKind, ClaimStatus,
    DRIVER_SCHEMA_VERSION, DriverPayload, QwenWorkClaim,
};
use super::driver::{
    AdapterBundle, AdapterCondition, AdapterConditionKind, AdapterStatusReport, AdapterSummary,
    ClaimResourceRef, ConditionStatus, DetectResult, DisableReport, DriverCtx, DriverPlan,
    EnableProgress, FrameworkCommand, FrameworkDriver, HostEnv, PreparedEnable,
    find_binary_in_path, is_executable,
};
use super::util::{bool_status, now_iso8601};

/// Manager-controlled sideload transactions and ownership verification.
pub(crate) mod tree;

const RES_PLUGIN: &str = "qwenwork_plugin";
const RES_PENDING: &str = "qwenwork_pending";
const MANIFEST: &str = ".qoder-plugin/plugin.json";
const RESTART: &str = "Restart QwenWorkCN, open a new local session, and check plugin activation in the UI. Activation and loaded hooks are not verified.";
const RUNTIME: &[&str] = &[
    MANIFEST,
    "hooks/hooks.json",
    "hooks/run-hook.sh",
    "hooks/shared/rewrite_hook.py",
    "hooks/shared/compress_response_hook.py",
    "hooks/shared/hook_utils.py",
    "hooks/shared/tool_categories.json",
    "scripts/lifecycle.py",
    "scripts/detect.sh",
    "scripts/install.sh",
    "scripts/uninstall.sh",
];

/// Manage QwenWorkCN sideloaded plugins in the current desktop user's data root.
pub struct QwenWorkDriver {
    platform: &'static str,
}

impl QwenWorkDriver {
    /// Construct a driver restricted to the actual host platform.
    pub fn new() -> Self {
        Self {
            platform: std::env::consts::OS,
        }
    }

    fn preflight(&self, ctx: &DriverCtx) -> Result<(String, bool), AdapterError> {
        let home = require_home(ctx)?;
        let (_, version) =
            application(&home, self.platform).map_err(|reason| invalid(ctx, reason))?;
        for binary in ["bash", "python3"] {
            if find_binary_in_path(binary).is_none() {
                return Err(invalid(
                    ctx,
                    format!("{binary} is required for QwenWork desktop hooks"),
                ));
            }
        }
        let result = ctx.ops.run_framework_cli(FrameworkCommand {
            program: "python3".to_string(),
            args: vec![
                "-I".into(),
                "-B".into(),
                "-c".into(),
                "import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)".into(),
            ],
            stdin: None,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            path_prepend: Vec::new(),
            timeout: Duration::from_secs(10),
        })?;
        if !result.success() {
            return Err(invalid(
                ctx,
                "Python 3.9+ is required for QwenWork desktop hooks",
            ));
        }
        if runtime_binary("tokenless", &home).is_none() {
            return Err(invalid(
                ctx,
                "tokenless binary not found; install Tokenless first",
            ));
        }
        Ok((version, runtime_binary("rtk", &home).is_some()))
    }
}

impl Default for QwenWorkDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkDriver for QwenWorkDriver {
    fn name(&self) -> &'static str {
        "qwenwork"
    }

    fn detect(&self, env: &HostEnv) -> DetectResult {
        let result = env
            .user_home
            .as_deref()
            .ok_or_else(|| "user home unavailable".to_string())
            .and_then(|home| application(home, self.platform));
        match result {
            Ok((app, version)) => DetectResult {
                detected: true,
                reason: format!(
                    "QwenWorkCN {} at {}; macOS local mode only; validated desktop version: 1.2.5{}",
                    version,
                    app.display(),
                    if version == "1.2.5" {
                        ""
                    } else {
                        "; compatibility is unverified"
                    }
                ),
            },
            Err(reason) => DetectResult {
                detected: false,
                reason,
            },
        }
    }

    fn probe_bundle(&self, root: &Path, declared_entry: Option<&str>) -> bool {
        declared_entry.is_none_or(|entry| entry == MANIFEST) && root.join(MANIFEST).is_file()
    }

    fn allowed_external_roots(&self, ctx: &DriverCtx) -> Vec<PathBuf> {
        data_root(ctx.user_home.as_deref()).into_iter().collect()
    }

    fn read_bundle(&self, ctx: &DriverCtx) -> Result<AdapterBundle, AdapterError> {
        if ctx.declared_plugin_id.as_deref().unwrap_or(&ctx.component) != "tokenless"
            || !self.probe_bundle(&ctx.resource_root, ctx.declared_bundle_entry.as_deref())
        {
            return Err(invalid(
                ctx,
                "QwenWork requires the tokenless .qoder-plugin/plugin.json bundle",
            ));
        }
        io_at(&ctx.resource_root, tree::source_digest(&ctx.resource_root))?;
        for relative in RUNTIME {
            let path = ctx.resource_root.join(relative);
            if !path.is_file() {
                return Err(invalid(
                    ctx,
                    format!("missing QwenWork runtime resource: {}", path.display()),
                ));
            }
        }
        let read_json = |relative: &str| -> Result<serde_json::Value, AdapterError> {
            let path = ctx.resource_root.join(relative);
            let contents = io_at(&path, std::fs::read(&path))?;
            let value: serde_json::Value = serde_json::from_slice(&contents)
                .map_err(|err| invalid(ctx, format!("invalid JSON {}: {err}", path.display())))?;
            if !value.is_object() {
                return Err(invalid(
                    ctx,
                    format!("expected JSON object: {}", path.display()),
                ));
            }
            Ok(value)
        };
        let manifest = read_json(MANIFEST)?;
        if manifest["name"] != "tokenless"
            || !manifest["version"]
                .as_str()
                .is_some_and(|v| !v.trim().is_empty() && !v.contains('@'))
        {
            return Err(invalid(
                ctx,
                "invalid or unstamped QwenWork plugin manifest",
            ));
        }
        read_json("hooks/hooks.json")?;
        read_json("hooks/shared/tool_categories.json")?;
        Ok(AdapterBundle {
            resource_root: ctx.resource_root.clone(),
            plugin_id: Some("tokenless".into()),
        })
    }

    fn plan_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<DriverPlan, AdapterError> {
        let (claim, _) = self.prepare_enable(bundle, ctx)?;
        let (target, pending, payload) = claimed_paths(&claim)?;
        let exists = io_at(target, tree::owned_digest(target))?.is_some();
        let mut actions = vec![
            format!(
                "{} owned QwenWork plugin at {}",
                if exists {
                    "adopt or upgrade"
                } else {
                    "sideload"
                },
                target.display()
            ),
            format!(
                "recover receipt-authorized replacement at {} if present",
                pending.display()
            ),
            format!(
                "QwenWorkCN {}; only 1.2.5 is validated",
                payload.app_version
            ),
            RESTART.to_string(),
        ];
        if !payload.rtk_available {
            actions.push("RTK not found: response compression remains available; command rewriting is unavailable".into());
        }
        Ok(DriverPlan {
            framework: self.name().into(),
            component: ctx.component.clone(),
            actions,
            register_command: None,
        })
    }

    fn prepare_enable(
        &self,
        bundle: &AdapterBundle,
        ctx: &DriverCtx,
    ) -> Result<(AdapterClaim, PreparedEnable), AdapterError> {
        let (app_version, rtk_available) = self.preflight(ctx)?;
        let data = data_root(ctx.user_home.as_deref()).ok_or_else(|| {
            invalid(
                ctx,
                "QwenWork requires an absolute, non-traversing data directory",
            )
        })?;
        let target = data.join("plugins-custom/tokenless");
        let pending = data.join(".anolisa-qwenwork-tokenless");
        io_at(&target, tree::check_path(&target))?;
        let mut tree_digests = vec![io_at(
            &bundle.resource_root,
            tree::source_digest(&bundle.resource_root),
        )?];
        if let Some(prior) = io_at(&target, tree::owned_digest(&target))? {
            tree_digests.push(prior);
        }
        // A pending slot is never adopted from its marker alone: recovery
        // must match a source or prior inventory persisted by the Manager.
        Ok((
            AdapterClaim {
                claim_schema: CLAIM_SCHEMA_VERSION,
                component: ctx.component.clone(),
                framework: self.name().into(),
                plugin_id: Some("tokenless".into()),
                adapter_type: ctx.adapter_type.clone(),
                enabled_at: now_iso8601(),
                resource_root: bundle.resource_root.clone(),
                bundle_digest: None,
                source_revision: None,
                materialized_files: Vec::new(),
                driver_schema: DRIVER_SCHEMA_VERSION,
                status: ClaimStatus::CleanupFailed,
                notices: Vec::new(),
                resources: vec![
                    ClaimResource {
                        id: RES_PLUGIN.into(),
                        purpose: "qwenwork_sideload".into(),
                        kind: ClaimResourceKind::ExternalPath { path: target },
                    },
                    ClaimResource {
                        id: RES_PENDING.into(),
                        purpose: "qwenwork_replacement".into(),
                        kind: ClaimResourceKind::ExternalPath { path: pending },
                    },
                ],
                driver_payload: DriverPayload::QwenWork(QwenWorkClaim {
                    plugin_resource: RES_PLUGIN.into(),
                    pending_resource: RES_PENDING.into(),
                    tree_digests,
                    app_version,
                    rtk_available,
                }),
            },
            PreparedEnable::None,
        ))
    }

    fn preserve_reenable_facts(
        &self,
        prior: &AdapterClaim,
        next: &mut AdapterClaim,
    ) -> Result<(), AdapterError> {
        let (old_target, old_pending, old) = claimed_paths(prior)?;
        let (target, _, _) = claimed_paths(next)?;
        if target != old_target {
            return Err(claim_error(
                prior,
                "data directory changed; restore the original configuration and disable first",
            ));
        }
        io_at(old_target, tree::authorized(old_target, &old.tree_digests))?;
        io_at(
            old_pending,
            tree::pending_complete(old_pending, &old.tree_digests),
        )?;
        if let DriverPayload::QwenWork(payload) = &mut next.driver_payload {
            payload.tree_digests.extend(old.tree_digests.clone());
            payload.tree_digests.sort();
            payload.tree_digests.dedup();
        }
        Ok(())
    }

    fn plan_reenable_cleanup(
        &self,
        prior: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<Vec<String>, AdapterError> {
        let (target, pending, payload) = claimed_paths(prior)?;
        validate_context(prior, ctx)?;
        io_at(target, tree::authorized(target, &payload.tree_digests))?;
        io_at(
            pending,
            tree::pending_complete(pending, &payload.tree_digests),
        )?;
        Ok(Vec::new())
    }

    fn validate_prepared_enable(&self, claim: &AdapterClaim) -> Result<(), AdapterError> {
        let (target, pending, payload) = claimed_paths(claim)?;
        io_at(target, tree::authorized(target, &payload.tree_digests))?;
        io_at(
            pending,
            tree::pending_complete(pending, &payload.tree_digests),
        )?;
        if let Some(revision) = &claim.source_revision {
            io_at(
                &claim.resource_root,
                tree::validate_source_revision(revision),
            )?;
        }
        Ok(())
    }

    fn apply_enable(
        &self,
        claim: &mut AdapterClaim,
        _: &PreparedEnable,
        ctx: &DriverCtx,
        _: &mut dyn EnableProgress,
    ) -> Result<(), AdapterError> {
        validate_context(claim, ctx)?;
        let (target, pending, payload) = claimed_paths(claim)?;
        ctx.ops.reconcile_qwenwork(
            target,
            pending,
            &payload.tree_digests,
            Some(&claim.resource_root),
        )?;
        let installed = io_at(target, tree::owned_digest(target))?
            .ok_or_else(|| claim_error(claim, "sideload missing after installation"))?;
        if let DriverPayload::QwenWork(payload) = &mut claim.driver_payload {
            payload.tree_digests = vec![installed];
        }
        if let DriverPayload::QwenWork(payload) = &claim.driver_payload {
            let mut notices = vec![(NoticeLevel::Info, RESTART.to_string())];
            if payload.app_version != "1.2.5" {
                notices.push((NoticeLevel::Warning, format!("QwenWorkCN {} is outside the validated 1.2.5 scope; compatibility is unverified", payload.app_version)));
            }
            if !payload.rtk_available {
                notices.push((NoticeLevel::Warning, "RTK not found: command rewriting is unavailable; response compression remains available".into()));
            }
            notices.push((NoticeLevel::Info, "Desktop sessions must resolve Bash, Python and Tokenless independently of the terminal PATH".into()));
            claim
                .notices
                .extend(notices.into_iter().map(|(level, text)| AdapterNotice {
                    when: NoticeWhen::PostEnable,
                    level,
                    text,
                    command: None,
                }));
        }
        claim.status = ClaimStatus::Enabled;
        Ok(())
    }

    fn status(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<AdapterStatusReport, AdapterError> {
        validate_context(claim, ctx)?;
        let (target, pending, payload) = claimed_paths(claim)?;
        let detected = self.detect(&HostEnv {
            user_home: ctx.user_home.clone(),
        });
        let (present, reason) = match tree::authorized(target, &payload.tree_digests) {
            Ok(Some(_)) => (
                ConditionStatus::True,
                "owned sideload inventory matches receipt".to_string(),
            ),
            Ok(None) => (ConditionStatus::False, "sideload absent".to_string()),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                ) =>
            {
                (ConditionStatus::Unknown, err.to_string())
            }
            Err(err) => (ConditionStatus::False, err.to_string()),
        };
        let (pending_exists, cleanup_reason) = match pending.try_exists() {
            Ok(false) if claim.status != ClaimStatus::CleanupFailed => {
                (false, "No pending tree cleanup".to_string())
            }
            Ok(_) => (
                true,
                format!(
                    "Retry enable or disable with the same data directory; recovery slot: {}",
                    pending.display()
                ),
            ),
            Err(err) => (
                true,
                format!("Cannot inspect recovery slot {}: {err}", pending.display()),
            ),
        };
        let condition = |kind, status, reason| AdapterCondition {
            kind,
            status,
            reason: Some(reason),
            resource: None,
        };
        Ok(AdapterStatusReport {
            summary: if claim.status == ClaimStatus::CleanupFailed || pending_exists {
                AdapterSummary::CleanupFailed
            } else if !detected.detected || present == ConditionStatus::False {
                AdapterSummary::Degraded
            } else {
                AdapterSummary::Unknown
            },
            conditions: vec![
                condition(
                    AdapterConditionKind::FrameworkDetected,
                    bool_status(detected.detected),
                    detected.reason,
                ),
                AdapterCondition {
                    kind: AdapterConditionKind::TreePresent,
                    status: present,
                    reason: Some(reason),
                    resource: Some(ClaimResourceRef {
                        id: RES_PLUGIN.into(),
                    }),
                },
                condition(
                    AdapterConditionKind::CleanupComplete,
                    bool_status(!pending_exists),
                    cleanup_reason,
                ),
                condition(
                    AdapterConditionKind::ActivationEnabled,
                    ConditionStatus::Unknown,
                    RESTART.into(),
                ),
                condition(
                    AdapterConditionKind::PluginResourcesLoaded,
                    ConditionStatus::Unknown,
                    "Desktop runtime loading cannot be verified from the filesystem".into(),
                ),
                condition(
                    AdapterConditionKind::VerificationSupported,
                    ConditionStatus::False,
                    "Only the local sideload can be verified".into(),
                ),
            ],
        })
    }

    fn disable(
        &self,
        claim: &AdapterClaim,
        ctx: &DriverCtx,
    ) -> Result<DisableReport, AdapterError> {
        validate_context(claim, ctx)?;
        let (target, pending, payload) = claimed_paths(claim)?;
        if let Err(error) = ctx
            .ops
            .reconcile_qwenwork(target, pending, &payload.tree_digests, None)
        {
            return Ok(DisableReport {
                cleanup_complete: false,
                messages: vec![error.to_string()],
            });
        }
        Ok(DisableReport {
            cleanup_complete: true,
            messages: vec!["QwenWork sideload removed or already absent. Restart QwenWorkCN and open a new local session to unload the plugin.".to_string()],
        })
    }
}

fn invalid(ctx: &DriverCtx, reason: impl Into<String>) -> AdapterError {
    AdapterError::InvalidAdapterInput {
        component: ctx.component.clone(),
        framework: "qwenwork".into(),
        reason: reason.into(),
    }
}

fn claim_error(claim: &AdapterClaim, reason: &str) -> AdapterError {
    AdapterError::InvalidAdapterInput {
        component: claim.component.clone(),
        framework: "qwenwork".into(),
        reason: reason.into(),
    }
}

fn io_at<T>(path: &Path, result: std::io::Result<T>) -> Result<T, AdapterError> {
    result.map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn require_home(ctx: &DriverCtx) -> Result<PathBuf, AdapterError> {
    ctx.user_home
        .clone()
        .ok_or_else(|| invalid(ctx, "desktop user home unavailable"))
}

fn absolute(value: PathBuf, home: &Path) -> Option<PathBuf> {
    let value = value
        .strip_prefix("~/")
        .map(|suffix| home.join(suffix))
        .unwrap_or(value);
    (value.is_absolute()
        && !value
            .components()
            .any(|c| matches!(c, Component::ParentDir)))
    .then_some(value)
}

fn data_root(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let canonical_home = home.canonicalize().ok()?;
    let path = absolute(
        std::env::var_os("TOKENLESS_QWENWORK_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".qwenworkcn")),
        home,
    )?;
    if let Ok(relative) = path.strip_prefix(home) {
        return Some(canonical_home.join(relative));
    }
    // An explicit external parent is caller-selected, matching Tokenless. The
    // root and every managed child are checked separately for symlinks.
    let parent = path.parent()?;
    let parent = crate::path_safety::canonicalize_nearest_existing(parent)?;
    Some(parent.join(path.file_name()?))
}

fn application(home: &Path, platform: &str) -> Result<(PathBuf, String), String> {
    if platform != "macos" {
        return Err("QwenWorkCN sideloading supports macOS local mode only".into());
    }
    let candidates = match std::env::var_os("TOKENLESS_QWENWORK_APP") {
        Some(value) => vec![
            absolute(value.into(), home)
                .ok_or("TOKENLESS_QWENWORK_APP must be an absolute path without '..'")?,
        ],
        None => vec![
            PathBuf::from("/Applications/QwenWorkCN.app"),
            home.join("Applications/QwenWorkCN.app"),
        ],
    };
    let app = candidates
        .iter()
        .find(|path| path.is_dir())
        .ok_or_else(|| format!("QwenWorkCN.app not found; checked {candidates:?}"))?;
    let info = plist::Value::from_file(app.join("Contents/Info.plist"))
        .map_err(|err| format!("cannot read QwenWork app metadata: {err}"))?;
    let dictionary = info
        .as_dictionary()
        .ok_or("invalid QwenWork app metadata")?;
    if dictionary
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        != Some("cn.qwenwork.desktop.mac")
    {
        return Err("unexpected Bundle ID; expected cn.qwenwork.desktop.mac".into());
    }
    let version = dictionary
        .get("CFBundleShortVersionString")
        .and_then(plist::Value::as_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing QwenWork version")?;
    Ok((app.clone(), version.into()))
}

fn runtime_binary(name: &str, home: &Path) -> Option<PathBuf> {
    let mut candidates = vec![home.join(".local/bin").join(name)];
    if name == "rtk" {
        candidates.extend([
            home.join(".local/lib/anolisa/libexec/tokenless/rtk"),
            home.join(".local/libexec/anolisa/tokenless/rtk"),
        ]);
    }
    candidates.extend([
        Path::new("/usr/local/bin").join(name),
        Path::new("/usr/bin").join(name),
    ]);
    if name == "rtk" {
        candidates.extend([
            PathBuf::from("/usr/local/libexec/anolisa/tokenless/rtk"),
            PathBuf::from("/usr/libexec/anolisa/tokenless/rtk"),
            PathBuf::from("/usr/lib/anolisa/tokenless/rtk"),
            home.join(".local/share/anolisa/tokenless/rtk"),
            home.join(".local/lib/anolisa/tokenless/rtk"),
        ]);
    }
    find_binary_in_path(name).or_else(|| {
        candidates
            .into_iter()
            .find(|path| path.is_file() && is_executable(path))
    })
}

fn claimed_paths(claim: &AdapterClaim) -> Result<(&Path, &Path, &QwenWorkClaim), AdapterError> {
    let DriverPayload::QwenWork(payload) = &claim.driver_payload else {
        return Err(claim_error(claim, "missing QwenWork receipt payload"));
    };
    let resource_path = |id: &str| match claim.resource(id).map(|r| &r.kind) {
        Some(ClaimResourceKind::ExternalPath { path }) => Ok(path.as_path()),
        _ => Err(claim_error(claim, "missing QwenWork path resource")),
    };
    let target = resource_path(&payload.plugin_resource)?;
    let pending = resource_path(&payload.pending_resource)?;
    let data = pending
        .parent()
        .ok_or_else(|| claim_error(claim, "missing QwenWork data root"))?;
    if claim.framework != "qwenwork"
        || claim.plugin_id.as_deref() != Some("tokenless")
        || target != data.join("plugins-custom/tokenless")
        || pending != data.join(".anolisa-qwenwork-tokenless")
        || payload.tree_digests.is_empty()
        || payload
            .tree_digests
            .iter()
            .any(|s| s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(claim_error(
            claim,
            "invalid QwenWork receipt paths or inventories",
        ));
    }
    Ok((target, pending, payload))
}

fn validate_context(claim: &AdapterClaim, ctx: &DriverCtx) -> Result<(), AdapterError> {
    let (target, _, _) = claimed_paths(claim)?;
    let data = data_root(ctx.user_home.as_deref())
        .ok_or_else(|| invalid(ctx, "invalid QwenWork data root"))?;
    if target != data.join("plugins-custom/tokenless") {
        return Err(claim_error(
            claim,
            "restore the original TOKENLESS_QWENWORK_HOME before managing this receipt",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::driver::{AdapterOps, CliOutput};
    use super::*;
    use anolisa_platform::fs_layout::FsLayout;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Env {
        fn set(values: &[(&'static str, &Path)]) -> Self {
            let saved = values
                .iter()
                .map(|(key, _)| (*key, std::env::var_os(key)))
                .collect();
            for (key, value) in values {
                // SAFETY: tests changing these QwenWork-specific keys hold ENV_LOCK.
                unsafe {
                    std::env::set_var(key, value);
                }
            }
            Self(saved)
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                // SAFETY: restoration happens before ENV_LOCK is released.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    struct Ops {
        python_ok: bool,
    }
    impl AdapterOps for Ops {
        fn run_framework_cli(&self, cmd: FrameworkCommand) -> Result<CliOutput, AdapterError> {
            assert_eq!(cmd.program, "python3");
            assert_eq!(&cmd.args[..2], &["-I", "-B"]);
            Ok(CliOutput {
                status: Some(if self.python_ok { 0 } else { 1 }),
                timed_out: false,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
        fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!()
        }
        fn copy_file(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!()
        }
        fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
            unreachable!()
        }
        fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
            unreachable!()
        }
        fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
            unreachable!()
        }
        fn read_file(&self, _: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
            unreachable!()
        }
        fn reconcile_qwenwork(
            &self,
            target: &Path,
            pending: &Path,
            digests: &[String],
            replacement: Option<&Path>,
        ) -> Result<(), AdapterError> {
            tree::reconcile(target, pending, digests, replacement)
        }
    }

    fn app(path: &Path, version: &str, binary: bool) {
        fs::create_dir_all(path.join("Contents")).unwrap();
        let info = plist::Value::Dictionary(
            [
                (
                    "CFBundleIdentifier".to_string(),
                    plist::Value::String("cn.qwenwork.desktop.mac".into()),
                ),
                (
                    "CFBundleShortVersionString".to_string(),
                    plist::Value::String(version.into()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        if binary {
            info.to_file_binary(path.join("Contents/Info.plist"))
                .unwrap();
        } else {
            info.to_file_xml(path.join("Contents/Info.plist")).unwrap();
        }
    }

    fn bundle(root: &Path) {
        for relative in RUNTIME {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                if path.extension().is_some_and(|ext| ext == "json") {
                    "{}"
                } else {
                    "# hook fixture\n"
                },
            )
            .unwrap();
        }
        fs::write(
            root.join(MANIFEST),
            r#"{"name":"tokenless","version":"1.0.0"}"#,
        )
        .unwrap();
    }

    fn context<'a>(
        home: &Path,
        root: &Path,
        layout: &'a FsLayout,
        ops: &'a dyn AdapterOps,
    ) -> DriverCtx<'a> {
        DriverCtx {
            component: "tokenless".into(),
            framework: "qwenwork".into(),
            layout,
            resource_root: root.to_path_buf(),
            user_home: Some(home.to_path_buf()),
            declared_plugin_id: Some("tokenless".into()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".into()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: Some(MANIFEST.into()),
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            no_accept_capabilities: false,
            dry_run: false,
            ops,
        }
    }

    #[test]
    fn desktop_detection_handles_xml_binary_versions_and_platform() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let application_path = home.join("custom ' 应用.app");
        let _env = Env::set(&[("TOKENLESS_QWENWORK_APP", &application_path)]);
        for binary in [false, true] {
            app(&application_path, "1.2.5", binary);
            assert_eq!(application(&home, "macos").unwrap().1, "1.2.5");
        }
        app(&application_path, "1.2.6", false);
        let driver = QwenWorkDriver { platform: "macos" };
        let report = driver.detect(&HostEnv {
            user_home: Some(home.clone()),
        });
        assert!(report.detected);
        assert!(report.reason.contains("compatibility is unverified"));
        assert!(
            application(&home, "linux")
                .unwrap_err()
                .contains("macOS local mode")
        );
        fs::write(application_path.join("Contents/Info.plist"), "invalid").unwrap();
        assert!(application(&home, "macos").is_err());
        let info = plist::Value::Dictionary(
            [(
                "CFBundleIdentifier".to_string(),
                plist::Value::String("other.app".into()),
            )]
            .into_iter()
            .collect(),
        );
        info.to_file_xml(application_path.join("Contents/Info.plist"))
            .unwrap();
        assert!(
            application(&home, "macos")
                .unwrap_err()
                .contains("Bundle ID")
        );
        let info = plist::Value::Dictionary(
            [(
                "CFBundleIdentifier".to_string(),
                plist::Value::String("cn.qwenwork.desktop.mac".into()),
            )]
            .into_iter()
            .collect(),
        );
        info.to_file_binary(application_path.join("Contents/Info.plist"))
            .unwrap();
        assert!(application(&home, "macos").unwrap_err().contains("version"));
    }

    #[test]
    fn receipt_lifecycle_dry_run_drift_and_source_removal() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let application_path = home.join("QwenWorkCN.app");
        let data = home.join("desktop data");
        let _env = Env::set(&[
            ("TOKENLESS_QWENWORK_APP", &application_path),
            ("TOKENLESS_QWENWORK_HOME", &data),
        ]);
        app(&application_path, "1.2.6", true);
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        fs::write(home.join(".local/bin/tokenless"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            home.join(".local/bin/tokenless"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let root = home.join("resources");
        bundle(&root);
        let layout = FsLayout::user(home.clone());
        let ops = Ops { python_ok: true };
        let mut ctx = context(&home, &root, &layout, &ops);
        let driver = QwenWorkDriver { platform: "macos" };
        let parsed = driver.read_bundle(&ctx).unwrap();
        ctx.dry_run = true;
        assert!(
            driver
                .plan_enable(&parsed, &ctx)
                .unwrap()
                .actions
                .iter()
                .any(|a| a.contains("1.2.6"))
        );
        assert!(!data.exists(), "dry-run must not create desktop files");
        ctx.dry_run = false;
        let (mut claim, prepared) = driver.prepare_enable(&parsed, &ctx).unwrap();
        if let DriverPayload::QwenWork(payload) = &mut claim.driver_payload {
            payload.rtk_available = false;
        }
        driver.validate_prepared_enable(&claim).unwrap();
        driver
            .apply_enable(&mut claim, &prepared, &ctx, &mut ())
            .unwrap();
        let serialized = toml::to_string(&claim).unwrap();
        assert_eq!(toml::from_str::<AdapterClaim>(&serialized).unwrap(), claim);
        assert!(
            claim
                .notices
                .iter()
                .any(|n| n.text.contains("RTK not found"))
        );
        assert!(
            claim
                .notices
                .iter()
                .any(|n| n.text.contains("compatibility is unverified"))
        );
        let report = driver.status(&claim, &ctx).unwrap();
        assert_eq!(report.summary, AdapterSummary::Unknown);
        assert!(
            report
                .conditions
                .iter()
                .any(|c| c.kind == AdapterConditionKind::ActivationEnabled
                    && c.status == ConditionStatus::Unknown)
        );
        fs::write(
            data.join("plugins-custom/tokenless/hooks/run-hook.sh"),
            "user change",
        )
        .unwrap();
        assert_eq!(
            driver.status(&claim, &ctx).unwrap().summary,
            AdapterSummary::Degraded
        );
        assert!(!driver.disable(&claim, &ctx).unwrap().cleanup_complete);
        fs::write(
            data.join("plugins-custom/tokenless/hooks/run-hook.sh"),
            "# hook fixture\n",
        )
        .unwrap();
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&application_path).unwrap();
        driver.disable(&claim, &ctx).unwrap();
        assert!(!data.join("plugins-custom/tokenless").exists());
    }

    #[test]
    fn bundle_and_python_preflight_fail_before_writes() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let application_path = home.join("QwenWorkCN.app");
        let data = home.join("data");
        let _env = Env::set(&[
            ("TOKENLESS_QWENWORK_APP", &application_path),
            ("TOKENLESS_QWENWORK_HOME", &data),
        ]);
        app(&application_path, "1.2.5", false);
        let layout = FsLayout::user(home.clone());
        let ops = Ops { python_ok: false };
        let root = home.join("bundle");
        bundle(&root);
        let ctx = context(&home, &root, &layout, &ops);
        let driver = QwenWorkDriver { platform: "macos" };
        let parsed = driver.read_bundle(&ctx).unwrap();
        assert!(
            driver
                .prepare_enable(&parsed, &ctx)
                .err()
                .unwrap()
                .to_string()
                .contains("Python 3.9+")
        );
        fs::remove_file(root.join("hooks/hooks.json")).unwrap();
        assert!(driver.read_bundle(&ctx).is_err());
        assert!(!data.exists());
        assert!(absolute(PathBuf::from("../escape"), &home).is_none());
        assert!(absolute(home.join("../escape"), &home).is_none());
        assert_eq!(
            absolute(PathBuf::from("~/custom"), &home),
            Some(home.join("custom"))
        );
    }
}
