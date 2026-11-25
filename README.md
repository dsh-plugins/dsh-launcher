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

- **Multi-version installs** keep each `@deepseek-ai/dsh` release from the npm registry in its own directory. To restore pnpm dependencies, stop instances using the version, then select **Repair files** under **Download → Create instance → Installed versions**.
- **Computer Use helpers** are stored by DSH version and platform under `<launcher data directory>/cua` by default. Downloading or repairing DSH `0.2.3-alpha.2` or later on native Windows or macOS automatically checks for the matching helper. The version task reports any helper failure as a warning. Linux and WSL only restore dependencies.
- **Settings → Computer Use** lets you turn off automatic helper downloads, move the helper directory, or download a release or pre-release before installing its DSH version. The launcher copies and verifies existing helpers before switching directories and preserves the original files. Manual downloads reuse complete helpers. The launcher verifies SHA-256 checksums for packages from the [community releases](https://github.com/dsh-plugins/dsh-cua-helper/releases). Windows helpers are unsigned and macOS helpers are ad-hoc signed without notarization. Existing application grants are preserved.
- **Computer Use at startup** uses the configured `helperPath` when one is set. For native instances with Computer Use enabled, the launcher can copy a complete, matching helper from the old pnpm directory to the new location without downloading anything. On Windows, choose a shorter root if the `.exe` or its runtime libraries exceed the path limit. Restart the instance after switching directories to use the new path.
- **Multi-instance management**: create several instances per version, each with its own name, profile, and runtime environment variables.
- **Three DSH_HOME modes**:
  - Reuse/share an existing DSH_HOME;
  - Automatically adopt the user's default `~/.dsh`;
  - Create a dedicated DSH_HOME per instance (created and registered under the app data directory automatically).
- **One-click launch**: pick an instance and a profile on the home page and start; the launcher parses the URL printed by `dsh web` and opens the DSH Web GUI in a dedicated webview window.
- **Environment overrides**: add or remove runtime environment variables on the instance settings page; they are injected into the child process at launch (`DSH_HOME` is reserved and always injected by the launcher from the selected DSH_HOME).
- **Plugin marketplace**: browse plugins published to the [DSH plugin market](https://dsh-plug.in/) (`https://dsh-plug.in/api/plugins.json`), search by name/description, and install them into any instance's profile:
  - **Three version channels**, distinguished by colored icon letters — **stable** (releases / npm `latest`, green **R**), **beta** (pre-releases / npm `next`, yellow **B**) and **alpha** (latest commit on GitHub, red **A**);
  - Plugin names accept either a single string or a localized object (`{"zh": "中文名称", "en": "English Name", "en-US": "...", "zh-CN": "..."}`); the market renders the current UI language, treats language keys case-insensitively, and searches across every language;
  - The install flow is a wizard: pick plugin → pick version channel/version → pick instance → pick profile → create a task → start installing;
  - Dependency build scripts are allowed automatically (`onlyBuiltDependencies: ['*']`), and the installed plugin is registered in the profile's `package.json` (`dsh.profile.bundles` + `dependencies`) plus a `cordis.patch.yml` insert row for non-bundle plugins.
- **Per-profile plugin management** (instance settings → Plugins tab): filter by profile to view the plugins installed in it, enable/disable individual plugins, and multi-select for batch enable/disable. Core `@deepseek-ai/*` packages are hidden.
- **Plugin update check** (instance settings → Plugins tab): checks the configured `format: 1` indexes first. The built-in plugin directory also uses its companion version index. For npm installs, if the target profile explicitly maps the package scope to a private registry, the launcher also checks that registry for releases the index may have missed. When both report the same version, it keeps the indexed install source. If the registry lookup fails, it can still use an indexed version eligible for the selected channel. When no matching version is indexed, public npm is queried for unscoped packages and for scoped packages listed in the built-in public catalog or explicitly routed to public npm. Private or ambiguous scoped routes block public lookups. Git installs update to the commit pinned by the index. Settings → Resources → Plugin updates selects Release (stable only) or Beta (includes prereleases). For npm registry lookups in Beta, the newer publish date of `latest` and `next` wins.
- **System tray**:
  - Left-click the launcher tray: opens and focuses the launcher window;
  - Right-click menu: a "Running profiles" submenu offers Open / Stop for each running instance, plus "Open launcher / Quit launcher".
  - Quitting the launcher terminates all instance processes so none are orphaned.
- **One tray icon per running instance** (issue #72, can be disabled in Settings):
  - The icon is the instance's own custom icon, falling back to the launcher icon;
  - Hovering shows `<instance> · <profile> · running (N active conversations)`; the count comes from the instance's DSH session index and refreshes every ~15s;
  - Left-click opens the instance's web GUI (or its terminal window for a TUI profile), focusing the window when it is already open;
  - Right-click offers "Open instance" / "Stop instance".
- **Close to tray** (can be disabled in Settings).
- **Launch at login** (Settings toggle, registered for real via the autostart plugin).
- **i18n**: Simplified Chinese / English; JSON locale files are discovered, hot-reloaded, and precompiled by `@intlify/unplugin-vue-i18n` through Vite.
- **WSL2 instances (experimental)**: create DSH instances inside a WSL2 distro — Node.js / pnpm / the pinned version are installed inside the distro automatically and the instance's DSH_HOME lives under the distro's `~/.dsh-launcher/homes`. Once created, HOME works like a local instance (profiles CRUD, plugin install/enable/disable/uninstall, skills, MCP, icons, logs, modpack export/import, embedded terminal); files are accessed through the `\\wsl$\` UNC share and a stopped distro is booted automatically.

## Modpacks

Import a local `.dspack`, download URL, or marketplace pack. The preview shows each profile's plugins and registries, plus optional plugin data, model providers, MCP servers, and skills. You can select individual resources and choose how to resolve conflicts. Supply credentials locally or confirm reuse of credentials on the target machine.

Export one profile or a selection of profiles from instance settings. Both exporters use PackForge manifest v6 and container v4 and let you select resources and declare tested DSH versions. Credential values stay on the target machine. Legacy `.tgz` packs with manifest v2/v3 and `.dspack` packs with manifest v4/v5 can still be imported.

To update an installed pack, stop the running instances that share its `DSH_HOME`, then choose **Update modpack** on the target instance. The preview lists changes and conflicts and keeps local changes by default. If the update requires a different DSH version, you can confirm a switch for a dedicated instance. For shared homes, adjust the affected instances to a compatible version first.

## Interface

- **Home**: left panel (instance status → linked instance/profile dropdowns → large launch button → instance list / instance settings); right side reserved for a news area.
- **Download**: sidebar with "Create instance / Download plugins"; the create page groups installable versions by stable/prerelease; clicking a version opens the naming page (instance name, DSH_HOME choice, "Start download" at the bottom). The plugins page is the marketplace; a three-step wizard (plugin → version channel → instance/profile) creates an install task.
- **Instances**: name, version, DSH_HOME, profile, runtime status and URL, edit/delete.
- **Instance settings → Plugins**: filter by profile, enable/disable plugins, multi-select batch enable/disable (`@deepseek-ai/*` core plugins hidden).
- **Settings**: five sections — Startup (launch behavior, data directory, DSH_HOME management, environment import, logs), Personalization (language, theme, news source, launcher icons), Network (proxy), Resources (SKILL repositories, plugin catalog sources, plugin updates) and About (version, update channel and check, repository / issues / releases links).

> For WSL instances, plugin and modpack dependency installs run pnpm inside the distro (pulling Linux platform binaries), and SKILL repository installs clone with the distro's own `git`; reading/writing files from the Windows side through UNC requires the distro to be running, which the launcher starts automatically.
>
> Known limitation: storage redirection (the storage tab) is not supported for WSL HOMEs yet (it would need to create and verify symlinks inside the distro); the page shows a capability notice instead of failing.

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
