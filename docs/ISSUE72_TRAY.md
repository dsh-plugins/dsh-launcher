# Issue #72: per-instance tray icons

Every running instance gets its own tray icon. The icon is the instance's own
custom icon (falling back to the launcher icon); hovering shows the instance
name, profile and active-conversation count; a left click opens or focuses the
instance window; a right-click menu can open or stop it. The launcher keeps its
own tray icon, and its "Running profiles" submenu remains available so a
disabled per-instance tray never strands a running instance.

Two Tauri behaviors determine the implementation and are easy to regress:

- `TrayIconBuilder::on_menu_event` appends to the app's **global** menu
  listener list, and `remove_tray_by_id` does not remove the listener it added.
  Registering one handler per tray would fire a single right-click N times for
  N running instances. The handlers are therefore registered once on the app
  handle (`tray::build_tray`) and route by menu-item id prefix.
- `TrayIconBuilder::on_tray_icon_event` is keyed **by tray id**, so a tray
  created after `build_tray` would have no click handler at all. The click
  listener is registered on the app handle too.
- A `TrayIcon` handle that outlives `remove_tray_by_id` keeps the native icon
  on screen. `AppState::instance_trays` stores ids only; the handle returned by
  `remove_tray_by_id` is dropped immediately.

The active-conversation count reads DSH's own session projection cache under
`<DSH_HOME>/storages/`, in either on-disk layout:

- `session_projcache/sessions/<session-id>.json` (newer, per record) —
  `record.rows.sessionStats.val.openStep`;
- `session_projcache.json` (older, aggregate) —
  `tables.sessions[<id>].rows.sessionStats.val.openStep`.

A non-null `openStep` alone is not "active": a crashed DSH leaves the last
checkpoint behind with `openStep` still set. A record only counts when it is
also fresh (its file was written within 5 minutes, or
`rows.sessionListMetadata.val.lastPromptAt` is within 5 minutes). The format is
internal to DSH, so every read is structure-agnostic and any failure — missing
directory, unreadable file, malformed JSON, a HOME where DSH never ran — drops
the count segment from the tooltip rather than surfacing an error. WSL homes are
only read when the distro is already up: a tooltip is not worth booting a distro
for.

## Manual smoke pass

Automated checks (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`,
`pnpm build`) cannot observe tray rendering or clicks, so this pass is manual on
a real desktop. Use disposable instances.

1. Give two instances different custom icons (instance settings → icon), start
   both, and confirm two extra tray icons appear, each showing its own icon.
2. Hover each tray icon: the tooltip is `<name> · <profile> · 运行中（N 个活跃对话）`.
   Start a conversation in one instance and confirm N rises within ~15s, and
   falls again when the turn finishes. An instance whose HOME never ran DSH
   shows the text without the count segment.
3. Left-click an instance tray icon: a closed instance window opens; clicking
   again focuses the already-open window. A TUI-profile instance opens its
   terminal window.
4. Right-click an instance tray icon: the menu offers "打开实例界面" / "终止实例",
   and choosing one acts **exactly once** (this is the global-listener
   regression). Stop the instance and confirm its tray icon disappears while
   the other instance's tray icon stays.
5. Left-click the launcher tray icon: the launcher window opens and focuses.
   Right-click it: the launcher menu still lists every running instance.
6. Turn off "每实例独立托盘图标" in Settings → 启动: all instance tray icons
   disappear immediately and the launcher tray's submenu still controls the
   running instances. Turn it back on: the icons return.
7. Change a running instance's icon in its settings: its tray icon updates
   without a restart.
8. Restart the launcher: no tray icons are left over from the previous run.

Expected platform caveats, not defects: on Windows 11 new tray icons land in
the overflow flyout until the user pins them; on macOS every icon is a menu-bar
item, so many instances occupy real menu-bar space; on GNOME the tray needs the
AppIndicator extension.
