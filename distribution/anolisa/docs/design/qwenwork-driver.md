# QwenWork desktop adapter

[中文版](qwenwork-driver_zh.md)

The `qwenwork` driver manages Tokenless sideload files for QwenWorkCN macOS local
sessions. It does not manage the desktop application's installation or activation.
The driver uses the existing AdapterManager lock, receipt, integrity, and logging
boundaries; it never executes packaged lifecycle scripts.

## Receipt and transaction

`DriverPayload::QwenWork` references two validated external resources: the plugin
at `plugins-custom/tokenless` and the recovery slot `.anolisa-qwenwork-tokenless`
under the selected data root. Its inventory digests authorize complete trees
before any mutation. Existing receipt formats are unchanged; an older CLI without
the new payload variant cannot manage these receipts. Complete disable before
downgrading to such a CLI or returning to standalone scripts.

The standalone Tokenless ownership marker remains compatible. Inventories include
relative names, file hashes, permissions, and directories. Initial enable may
adopt a marker-verified standalone installation; re-enable also checks the prior
receipt, so rewriting the marker cannot silently redefine existing ownership.
Only files matching the package inventory may be copied. Source bundles must
already contain self-contained hooks rather than source-checkout symlinks.

The Manager's `reconcile_qwenwork` operation checks both destination boundaries
and the replacement source. It stages a complete copy outside the application's
plugin scanner, verifies it, then uses no-replace rename for first installation
or atomic exchange for replacement. The receipt starts as `cleanup_failed` and
retains old and new digests until publication and cleanup both succeed.

Recovery accepts only receipt-authorized inventories. During removal the marker
is deleted last; a partially deleted recovery tree may contain a matching subset
of its original inventory. An empty recovery slot can be removed after a crash
between deleting the marker and removing the directory. Extra or changed entries
stop cleanup. An unpublished temporary copy left by abrupt process termination
is outside the scanner and is not adopted or recursively cleaned on a later run.

## Observation and compatibility

Detection reads the app's plist without spawning a framework CLI. Enable performs
bounded Python-version preflight and checks required binaries. Version 1.2.5 is
the existing Tokenless validation target; other versions produce a notice.
The environment overrides match Tokenless's application and data-root names.
Changing the data-root configuration requires restoring it before receipt cleanup.

`TreePresent` reports full copied-tree integrity. The Manager adds source revision
and package integrity conditions. `ActivationEnabled` and `PluginResourcesLoaded`
remain `unknown`, and a sound sideload still has an `unknown` summary. The CLI
reports `Sideloaded`, with UI/restart instructions; JSON retains these distinctions.
Disable needs neither the desktop application nor the original component bundle.

## Validation boundary

Rust tests exercise plist formats, platform rejection, marker compatibility with
the existing Python installer, adoption, drift protection, interrupted replacement
and deletion, receipt serialization, and Manager path boundaries. CLI integration
tests reject mutations on Linux and exercise the full flow on macOS. Those tests
use fixture bundles; actual desktop loading and Hook behavior require a macOS
QwenWorkCN session and are not established by the filesystem tests.
