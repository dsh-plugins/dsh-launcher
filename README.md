<div align="center">

<img src="docs/banner.png" width="1024" height="512" alt="DSH Launcher banner">

# DSH Launcher

**A desktop launcher for running multiple [DeepSeek Harness (DSH)](https://github.com/deepseek-ai/deepseek-harness) versions and instances side by side.**

English | [简体中文](README.zh_CN.md)

</div>

Tauri 2 + Vue 3 + TypeScript + Sass + vue-router + vue-i18n + Arco Design Vue.

## Preview

![Preview](docs/img.png)

## Features

- **Multi-version installs**: query the npm registry and install multiple `@deepseek-ai/dsh` versions into isolated directories that never interfere with each other.
- **Multi-instance management**: create several instances per version, each with its own name, profile, and runtime environment variables.
- **Three DSH_HOME modes**:
  - Reuse/share an existing DSH_HOME;
  - Automatically adopt the user's default `~/.dsh`;
  - Create a dedicated DSH_HOME per instance (created and registered under the app data directory automatically).
- **One-click launch**: pick an instance and a profile on the home page and start; the launcher parses the URL printed by `dsh web` and opens the DSH Web GUI in a dedicated webview window.
- **Environment overrides**: add or remove runtime environment variables on the instance settings page; they are injected into the child process at launch (`DSH_HOME` is reserved and always injected by the launcher from the selected DSH_HOME).
- **Plugin marketplace**: browse plugins published to the [DSH plugin market](https://dsh-plug.in/) (`https://dsh-plug.in/api/plugins.json`), search by name/description, and install them into any instance's profile:
  - **Three version channels**, distinguished by colored icon letters — **stable** (releases / npm `latest`, green **R**), **beta** (pre-releases / npm `next`, yellow **B**) and **alpha** (latest commit on GitHub, red **A**);
  - The install flow is a wizard: pick plugin → pick version channel/version → pick instance → pick profile → create a task → start installing;
  - Dependency build scripts are allowed automatically (`onlyBuiltDependencies: ['*']`), and the installed plugin is registered in the profile's `package.json` (`dsh.profile.bundles` + `dependencies`) plus a `cordis.patch.yml` insert row for non-bundle plugins.
- **Per-profile plugin management** (instance settings → Plugins tab): filter by profile to view the plugins installed in it, enable/disable individual plugins, and multi-select for batch enable/disable. Core `@deepseek-ai/*` packages are hidden.
- **System tray**:
  - Double-click: opens the most recently focused instance's profile page; with a single running instance opens it directly, otherwise shows the launcher;
  - Right-click menu: a "Running profiles" submenu offers Open / Stop for each running instance, plus "Open launcher / Quit launcher".
  - Quitting the launcher terminates all instance processes so none are orphaned.
- **Close to tray** (can be disabled in Settings).
- **Launch at login** (Settings toggle, registered for real via the autostart plugin).
- **i18n**: Simplified Chinese / English; JSON locale files are discovered, hot-reloaded, and precompiled by `@intlify/unplugin-vue-i18n` through Vite.
- **WSL2 instances (experimental)**: create DSH instances inside a WSL2 distro — Node.js / pnpm / the pinned version are installed inside the distro automatically and the instance's DSH_HOME lives under the distro's `~/.dsh-launcher/homes`. Once created, HOME works like a local instance (profiles CRUD, plugin install/enable/disable/uninstall, skills, MCP, icons, logs, modpack export/import, embedded terminal); files are accessed through the `\\wsl$\` UNC share and a stopped distro is booted automatically.

## Interface

- **Home**: left panel (instance status → linked instance/profile dropdowns → large launch button → instance list / instance settings); right side reserved for a news area.
- **Download**: sidebar with "Create instance / Download plugins"; the create page groups installable versions by stable/prerelease; clicking a version opens the naming page (instance name, DSH_HOME choice, "Start download" at the bottom). The plugins page is the marketplace; a three-step wizard (plugin → version channel → instance/profile) creates an install task.
- **Instances**: name, version, DSH_HOME, profile, runtime status and URL, edit/delete.
- **Instance settings → Plugins**: filter by profile, enable/disable plugins, multi-select batch enable/disable (`@deepseek-ai/*` core plugins hidden).
- **Settings**: language, close to tray, launch at login, DSH_HOME management.

> For WSL instances, plugin and modpack dependency installs run pnpm inside the distro (pulling Linux platform binaries); reading/writing files from the Windows side through UNC requires the distro to be running, which the launcher starts automatically.

## Development

Prerequisites: Node ≥ 20, pnpm, stable Rust (MSVC toolchain), WebView2.

```bash
pnpm install
pnpm tauri dev      # dev mode (Vite frontend + debug backend)
pnpm tauri build    # bundle (produces exe / msi / nsis installers)
```

Preview the frontend in a browser without the backend (mock layer backed by localStorage):

```bash
pnpm dev            # open http://localhost:1420
```

## Runtime data

- Launcher config and data: `%APPDATA%\in.dsh-plug.dsh-launcher\`
  - `config.json`: DSH_HOMEs / versions / instances / settings
  - `versions/<version>/`: isolated install directory per version
  - `homes/<instance name>/`: dedicated DSH_HOME (when chosen)
  - `logs/<instance id>.log`: instance runtime logs

## Architecture

- `src/`: Vue 3 frontend (pages, store, API wrapper, i18n)
- `src/api/index.ts`: unified API layer — `invoke` under Tauri, localStorage mock in a plain browser
- `src-tauri/src/`
  - `config.rs`: config model and atomic persistence
  - `commands.rs`: all Tauri commands (CRUD / version installs / instance start-stop / settings)
  - `plugins.rs`: plugin marketplace — catalog fetch, per-channel versions (npm dist-tags + GitHub commits), install task, profile manifest registration and enable/disable
  - `tasks.rs`: background task system (create-instance / install-plugin) with progress and log events
  - `process.rs`: instance process management (spawn / kill / URL parsing / env injection / logs)
  - `tray.rs`: system tray and dynamic menu
  - `windows.rs`: instance webview window management

## License

MIT
