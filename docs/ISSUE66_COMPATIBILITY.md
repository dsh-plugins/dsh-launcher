# Plugin compatibility diagnostics (issue #66)

The read-only **Check Compatibility** action inspects the selected instance and
profile. **Compatibility launch** is opt-in; normal Start, deep links and
environment-change restarts remain advisory. A report records the selected DSH
version and runtime, check time, status, findings, actions, unresolved issues,
whether a process was spawned, and whether a TUI PTY handoff is pending.
Actions distinguish applied disables from uncertain write outcomes. `started`
does not mean the app is ready. The older `check_instance_health` contract
remains available for legacy consumers; the new workflow uses
`check_plugin_compatibility`.

## Evidence and decisions

- A duplicate **enabled** Cordis entry ID across bundle and user patch layers
  is a confirmed collision. There is no automatic victim.
- An enabled plugin's declared peer dependency on another enabled plugin is
  compared with that peer's installed version. A proven mismatch is confirmed,
  but has no automatic victim.
- An installed package's `peerDependencies["@deepseek-ai/dsh"]` or
  `engines.dsh` semver range is compared with the selected installed CLI.
  Prerelease compatibility must be explicit in the range; without that evidence
  the result is unknown, not an incompatibility verdict.
- An installed package may declare `dsh.runtime.surface` (array of `web`,
  `tui`, `other`) and `dsh.runtime.platform` (array of `windows`, `wsl`).
  Exclusion of the selected runtime is a confirmed incompatibility. Missing
  metadata is a warning. Marketplace fields and plugin names are not proof.
- Existing duplicate/mixed core-tree findings remain report-only. Missing or
  malformed required manifests and patches make the scan partial or failed;
  unknown dynamic/unsupported patch entries cannot certify compatibility.
  Group-targeted inserts and name-guarded overrides require loader semantics
  not reproduced by this inspector and remain unassessable.
- A confirmed version/runtime mismatch may be disabled only when the enabled
  package has exactly one, unambiguous user `insert` entry, is not a core or
  required bundle, and no duplicate ID or package mapping exists. Bundle
  overrides, collisions, and transitive-only/uncertain provenance are never
  chosen automatically.

Compatibility launch refuses to start if the scan is incomplete or confirmed
issues remain. It serializes inspection, profile patch changes, reinspection,
and Web spawn with the profile install/uninstall and manual toggle lock. A
recoverable `.issue66.bak` copy protects a patch write; if a backup remains,
recover it manually before retrying. Disabled entries remain disabled after a
failed spawn, and can be re-enabled in Instance Settings > Plugins. For TUI,
the window's delayed PTY creation consumes a matching approval only after a
fresh inspection under the same lock. The PTY then emits a final spawn/failure
report to Home. Do not edit or run a profile in another
process outside the launcher during this operation; external edits are not
serialized by the launcher lock.

## Manual smoke pass

Use disposable instances and profiles, not a fixed DSH installation. For each
Windows and WSL runtime, check both a Web and a TUI profile:

1. Start normally with a clean profile. Verify the full advisory report is
   visible on Home and no profile file changes. Repeat through a deep link and
   an environment-change restart; both must remain ordinary launches.
2. Add an installed plugin with an explicit incompatible DSH prerelease or
   runtime constraint in a unique user `insert` row. Check Compatibility in
   Instance Settings > Plugins; verify that the check is read-only. Switch
   profiles while a check is pending and ensure its late result is discarded.
3. Stop all instances sharing the HOME. Use Compatibility launch. Verify only
   the selected row receives `disabled: true`, nested configuration remains,
   the after-report records the action, and Web spawn or TUI PTY startup is
   reported separately from readiness. Re-enable the plugin manually.
4. Repeat with duplicate IDs, malformed patches, unreadable manifests, and a
   running shared-HOME instance. Compatibility launch must not mutate or spawn
   when the conflict lacks a safe victim or the scan is incomplete.
5. For WSL, stop the distro before the read-only check and both launch modes;
   verify it boots before UNC reads. For TUI, edit the patch between window
   creation and PTY startup; the handoff must fail without spawning.
