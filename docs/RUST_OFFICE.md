# Rust Office runtime (experimental)

This is an opt-in, native Rust first slice, not a replacement for Microsoft
Office or a full document rendering engine. It does not launch Python or Node,
install packages, download runtime engines, execute document macros, or call
LibreOffice. Existing DSH Office skills are unchanged: upstream skills that
request Python are **not** made Python-free by installing this server.

## Enable in DSH Launcher

1. Open an instance's **MCP** settings and select its profile (or deliberately
   select global scope).
2. Choose **Add Rust Office (experimental)** and select a dedicated existing
   directory. The server can read supported files and create new files there;
   do not select your home directory or another folder containing secrets.
3. Wait for the local runtime self-check (handshake, tool discovery and capability
   query only, no document reads or writes). It has a 15-second deadline and
   64 KiB per-output-stream limit. A failed check reports an error without saving.
   AppImage checks its current inner binary, not the outer mount wrapper.
   Review the generated stdio command, arguments and directory in the normal
   MCP form. Save explicitly, then restart DSH. Canceling the form writes no
   configuration. If the name already exists, choose a distinct server name.
4. Ask DSH to use the `office-rust` MCP tools for the supported operations. Tool
   descriptions and `office_info` state the limits; no upstream skill is replaced.

The preset uses the existing `@deepseek-ai/dsh-mcp-client` patch-layer integration
and can be disabled or removed in that same MCP page. It is not enabled on
existing instances automatically. Native local instances only: the built-in
preset rejects WSL homes because Windows and Linux executable/workspace paths
are not interchangeable. For WSL, build the standalone Rust binary in the distro
and configure its Linux paths manually.

## Runtime entry points

The packaged launcher's early entry point runs before GUI initialization, logging,
deep links and the single-instance handler:

```sh
dsh-launcher --office-mcp --workspace /absolute/dedicated/office-folder
```

The GUI-independent crate can also be built without Tauri's system dependencies:

```sh
cargo build --manifest-path src-tauri/Cargo.toml -p dsh-office --locked
cargo test --manifest-path src-tauri/Cargo.toml -p dsh-office --locked
src-tauri/target/debug/dsh-office --workspace /absolute/dedicated/office-folder
```

No network listener is started. stdin/stdout carry newline-delimited JSON-RPC
MCP messages only; startup diagnostics go to stderr. Each client starts its own
server. AppImage presets use the outer AppImage path rather than the temporary
mount path. Moving/removing the launcher requires updating the saved command.

## Supported scope

- Capabilities and explicit limits through `office_info`.
- Basic DOCX creation from paragraphs and bounded text extraction.
- Basic XLSX creation from typed cells and bounded workbook reading.
- Semantic HTML preview of the supported paragraph/typed-cell content. This is
  not Office page-layout rendering; see the limitations below.
- New files only; existing documents are never edited or overwritten.

Unsupported: PPTX, PDF import/export, original Office layout rendering or layout guarantees,
formula calculation, full-fidelity round-trip editing, charts, embedded objects,
macros, encrypted documents, and arbitrary scripts. The reader intentionally
accepts a conservative subset: it rejects all `.bin` package parts (including
otherwise benign printer settings), all external relationships (including ordinary
hyperlinks), percent-encoded relationship targets and non-UTF-8 XML. A valid
Office document can therefore be unsupported; rejection does not mean the
original document is corrupt. Formula text read from a
workbook is not a calculated result. No fallback engine is silently invoked. HTML previews contain no scripts, remote
resources, navigable links or embedded active document content; a restrictive
CSP provides additional protection. They deliberately list sparse XLSX cells
with coordinates/types, rather than pretending to reproduce the original grid
layout, formatting, charts or print pagination.

## Security boundary

MCP transport is not a permission sandbox by itself. This runtime enforces a
fixed root, validates relative file paths and archive/XML input, bounds resource
usage, and uses capability-based filesystem operations. It never grants access
to DSH_HOME just because the server was configured there. Output is published as
new files only. The operating-system account still needs ordinary filesystem
permissions; selecting a root explicitly grants this server access within it.

This is not a defense against a malicious user controlling the same OS account
or against resource consumption by an arbitrary replacement executable edited
into the generic MCP form. Review MCP changes as executable configuration.

## Verification and development status

Existing CI's Rust workspace format, clippy and test steps cover this crate as
well as the launcher. The isolated [official DSH bridge smoke](../ci/office-dsh/README.md)
uses pinned published DSH 0.2.2-rc.1 packages, the actual tool runtime and native
executor to discover tools in the assembled model prompt and execute them. It
uses no API keys or inference, and does not claim to test model tool selection
or the complete DSH agent/session UI. CI runs the standalone route on each
platform and the release Launcher route on Windows. Before release, verify a real MCP handshake and round-trip
through DSH on Windows, macOS, Linux and AppImage; validate generated documents
in Office/LibreOffice separately. A ZIP/XML round-trip is not visual rendering QA.

The draft PR records the exact checks run and any blockers. Standalone Rust
checks and a frontend build do not replace a packaged launcher/DSH smoke test.
In particular, Windows GUI-subsystem inherited pipes and AppImage launch paths
need verification in real packaged applications before release.

### Tool examples

Arguments for `docx_create`:

```json
{"path":"notes.docx","paragraphs":["Meeting notes","First point\nSecond point"]}
```

Arguments for `xlsx_create`:

```json
{"path":"budget.xlsx","sheets":[{"name":"Budget","rows":[["Item","Cost"],["Paper",12.5],["Paid",true]]}]}
```

Read either file with `{"path":"notes.docx"}` or
`{"path":"budget.xlsx"}` on its matching read tool. XLSX reading returns sparse
cells with addresses, types and values, not a dense rectangular array. Strings
starting with `=` remain literal strings when creating workbooks. Existing
formula cells are returned as formula text plus their untrusted cached value;
shared/array formulas are not expanded. Numeric date serials are not converted
using cell styles.

Current bounds: 1 MiB per protocol request/response; 16 MiB input/output files;
256 ZIP entries and 32 MiB declared expansion; 2 MiB, 100,000 nodes and depth
128 per XML part; at most 64 attributes per element and 128 namespace
declarations per part; 256 KiB total text, 32 KiB per string; 2,000 paragraphs; 16 sheets, 1,000 rows
and 256 columns per sheet, 20,000 cells per workbook. Limits are intentionally
conservative and reported by `office_info`. Unsupported or oversized inputs
fail explicitly rather than silently truncating content. Existing parent
folders are required. A disk write failure may leave a partial new file; the
server reports that possibility and never deletes or overwrites it automatically.

XLSX strings also respect Excel's 32,767 UTF-16-unit cell limit. Numeric creation
rejects values beyond Excel's finite normal numeric range or 15 significant
decimal digits; provide long identifiers and precision-sensitive values as text.

### Semantic preview example

Call `office_preview` with:

```json
{"path":"notes.docx","output":"notes-preview.html"}
```

The source may also be an XLSX file. Open the new HTML file in a browser for a
readable content check. Paragraph whitespace and sparse cell coordinates/types
are retained; formula text is visibly inert and cached values may be stale.
This does not certify the source's Office pagination or appearance. Preview
creation uses the same capability-confined, create-new filesystem operations
as document creation, has a 16 MiB output cap, and launches no browser or
external process. The tool only returns the output path and descriptive metadata.

### Future layout-rendering boundary

The official Node LibreOffice Kit can provide real PNG/PDF output, but it is not
loaded by this Rust server. A child Node/native engine does not inherit Rust
capability-based filesystem confinement. Enabling one safely requires an
explicit sandbox/resource boundary, trusted staged input/output, bounded font
access, and process-tree cancellation tests. Native helpers and Linux WASM also
have different lifetime requirements. Existing DSH preview services may be used
through their normal authorization flow; semantic HTML here is not a substitute
for that layout-rendering engine.
