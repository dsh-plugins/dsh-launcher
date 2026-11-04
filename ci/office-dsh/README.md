# Rust Office × official DSH client smoke

This isolated, keyless integration test uses **published official packages**, not
an in-repository bridge mock or hand-written JSON-RPC client. It mounts Cordis
`Context`, DSH `SystemPrompt`, `ToolRuntime`, the native tool executor, and
`@deepseek-ai/dsh-mcp-client`, then starts the actual compiled Rust executable
using MCP stdio.

## Run

From the launcher directory, with Node.js 24.9 or newer:

```sh
npm --prefix ci/office-dsh ci --ignore-scripts --no-audit --no-fund
cargo build --manifest-path src-tauri/Cargo.toml --locked -p dsh-office
node ci/office-dsh/smoke.mjs src-tauri/target/debug/dsh-office --standalone
```

For a compiled launcher executable, omit `--standalone`. The test then invokes
its real `--office-mcp` startup route:

```sh
node ci/office-dsh/smoke.mjs /absolute/path/to/dsh-launcher
```

On Windows, use the corresponding `.exe` paths. The executable is resolved
relative to the current working directory. All test documents are created under
an isolated temporary workspace and removed after the bridge is disposed.

## What it proves

- All six `mcp__office-rust__*` tools are registered and projected into the
  assembled model prompt, with their schemas and descriptions.
- Calls use the public `ctx.tools.execute()` batch API and JSON-encoded model
  arguments, exercising the native executor, bridge, real MCP transport, Rust
  handler, canonical result, and model-visible result projection.
- DOCX round trips preserve Unicode, XML-special characters, whitespace, and
  empty paragraphs.
- XLSX round trips preserve typed cells, sparse nulls, empty sheets, and literal
  formula-looking text without formula evaluation.
- DOCX/XLSX semantic HTML previews escape script/image-shaped input, retain
  Unicode and typed-cell content, disclose fidelity and cached-formula limits,
  and leave source files untouched. A malformed input creates no preview file.
- Preview outputs also reject overwrite and workspace-escape attempts.
- Rust overwrite and workspace traversal rejections become DSH tool failures,
  and the existing/outside files remain unchanged.
- Missing or malformed files, invalid arguments, and unknown public tool names
  return errors. The current bridge forwards malformed arguments to the Rust
  handler; this test does not claim DSH schema validation rejects them first.
- A failed call does not break later calls, and disposal unregisters the tools.

## Pinned compatibility and limits

The package and lockfile pin the official **DSH 0.2.2-rc.1** release-candidate
cohort and **Cordis 4.1.0**. DSH 0.2.2-rc.1 is published under the `next` dist-tag;
`latest` for several DSH packages still points to 0.0.1-rc.1. Do not replace these
exact versions with unqualified installs or mix the two API generations. The
lockfile also pins the MCP client SDK (2.2.0) and all transitive dependencies.

These dependencies belong only to this smoke test. They do not enter the
launcher's root package, lockfile, frontend bundle, or Rust Office runtime.
The test uses only public installed package exports and does not need
`dsh-tool-testkit`, the official source checkout, Python, or an Office app.

No model provider, API key, real inference, or DSH agent/session loop is used.
This verifies official DSH tool registration and execution, not model selection
of the tools, graphical launcher workflows, Office UI rendering, or full-fidelity
editing. Standalone mode does not verify the launcher's embedded executable
route; run the launcher command above to cover that route separately.
