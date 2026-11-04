# Task-aware exit protection

Early-loading cancellation also checks a spawned Host before recording cancellation.

Normal launcher exit (main-window Close with close-to-tray disabled, the tray
Quit action, and Tauri ExitRequested) checks all running instances and launcher
installation/download jobs before cleanup. Stop and Delete instance also check
the affected instance. Cancel keeps it running and returns cancellation to the
caller, so a settings workflow cannot treat cancellation as successful stop and
restart it. Closing an instance window merely detaches its view. Close-to-tray
remains non-destructive and does not ask.

## Evidence, not process counting

Native Web profiles whose installed CLI advertises the `--patch <path>`
patch-list contract receive an ephemeral overlay. The bundled zero-dependency
`exit-inspector.mjs` plugin reads the actual Host services:

- all live agents, including subagents, generating/tool-running/approval-wait
  agents (`status === 'running'`);
- both inbox queues (`nextTurn`, `nextStep`);
- unowned and per-agent background jobs (`running`, `stopping`);
- `schedule` activity from `workspace/session-activity` for loaded agents only.

These match upstream `apps/desktop-host/src/update-tasks.ts` and
`quit-inspection.ts` (reviewed at
`fcc0ad462172522aaf5c93a7b2e01f6b9f0b0159`). A running process with a fresh idle
report exits without confirmation. Historical session caches, `openStep`,
filesystem modification times, and tray activity estimates are never used as
exit authority. A launcher startup failure is not proof that the Host has no
work.

The plugin emits a versioned, run-nonce-bound, start-timestamped summary over its existing stdout
pipe every 500 ms. The launcher consumes these frames without adding them to
logs. Inspection taking over one second becomes unknown; active work is re-read
after asynchronous schedule inspection. Buffered delivery does not renew freshness.
It accepts only the current process generation's frames; invalid frames,
missing services, missing bridge, stalled inspections, or samples older than two
seconds from inspection start are **unknown**, not idle or asserted active. Overlay files are unique
per launch, do not alter the user's profile, and are removed on normal process
cleanup or failed startup. An abrupt launcher/OS crash may leave an inert overlay
directory; it is never reused as evidence. No network listener or cookie/token
exchange is introduced. The nonce prevents accidental cross-run confusion; it
is not a security boundary against malicious plugins in the same Host.

## Confirmation behavior

Native English/Chinese confirmation distinguishes interrupted work, armed
reminders, and unknown state. Multi-instance reasons are combined. Repeated Quit
requests coalesce; stop decisions cannot stack on an existing decision. Quit
confirmation is accepted once, then cleanup runs through the existing Exit
handler. A failed dialog cancels, not quits.

This is sampled inspection, not atomic task admission: work can start just after
a sample/confirmation, as with any asynchronous UI confirmation. This version
does not claim a Host-level restart lock or graceful task draining. TUI, WSL,
older/bundled CLIs without detectable patch support, and already-running Hosts
without the bridge use the explicit **unknown state** confirmation. Host IPC for
those carriers and OS-session-shutdown bypass are not implemented. Forced OS
termination cannot be prevented.

## Validation

- `node --test ci/exit-inspector.test.mjs`: Host semantics, both queues, subagents,
  global/owned jobs, reminders, missing/failed services, per-run frames, disposal.
- `cargo test --manifest-path src-tauri/Cargo.toml exit_protection`: strict frame
  schema, old-run rejection, expiry, and multi-instance aggregation.
- Native integration checklist: idle Web exit, running/approval-wait work,
  scheduled-only work, mixed idle/active/unknown instances, repeated Quit,
  Cancel then Quit, Stop cancellation during network-settings restart, instance
  crash/restart while inspecting, close-to-tray, unsupported TUI/WSL, and hidden
  windows. Rust compilation and native UI validation require the platform build
  environment; Node unit tests are not a substitute.
