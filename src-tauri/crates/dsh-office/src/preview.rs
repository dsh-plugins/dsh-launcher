//! Inert HTML presentation of the existing bounded readers, not an OOXML
//! renderer. Document-controlled strings are only ever written as text nodes.
use crate::office::{PathArgs, PreviewArgs};
use crate::workspace::{Workspace, MAX_FILE_BYTES};
use crate::{docx, xlsx};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub(crate) const MAX_PREVIEW_BYTES: usize = MAX_FILE_BYTES as usize;
const SEMANTIC_WARNING: &str = "Semantic content preview only, not full-fidelity Office rendering or PDF export. Original formatting and page layout are not preserved.";
const SPARSE_WARNING: &str = "Stored cells are listed by coordinate, not expanded into a rectangular grid. Unlisted coordinates have no stored cell. String values use JSON quotes and escapes; an empty string differs from an absent value.";
const FORMULA_WARNING: &str = "Formulas are inert text and are never evaluated. Formula values are cached values that may be absent or stale. Shared and array formulas are not expanded.";

// Deserialize the existing readers' results instead of adding a second OOXML
// parser or relaxing any of their package, XML, text, and cell limits.
#[derive(Deserialize)]
struct Document {
    paragraphs: Vec<String>,
    warnings: Vec<String>,
}
#[derive(Deserialize)]
struct Workbook {
    sheets: Vec<Sheet>,
    formula_cells: usize,
    warnings: Vec<String>,
}
#[derive(Deserialize)]
struct Sheet {
    name: String,
    cells: Vec<Cell>,
}
#[derive(Deserialize)]
struct Cell {
    cell: String,
    #[serde(rename = "type")]
    kind: String,
    value: Value,
    formula: Option<Formula>,
}
#[derive(Deserialize)]
struct Formula {
    text: String,
    kind: String,
    shared_index: Option<String>,
    reference: Option<String>,
}

pub(crate) fn create(workspace: &Workspace, args: PreviewArgs) -> Result<Value, String> {
    let format = Path::new(&args.path)
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("preview input must be a workspace-relative .docx or .xlsx path")?;
    let source = PathArgs {
        path: args.path.clone(),
    };
    let (html, counts, warnings) = match format {
        "docx" => {
            let document: Document = serde_json::from_value(docx::read(workspace, source)?)
                .map_err(|e| format!("invalid DOCX reader result: {e}"))?;
            let mut warnings = vec![SEMANTIC_WARNING.to_owned()];
            warnings.extend(document.warnings);
            let mut html = Html::new();
            html.start(&args.path, &warnings)?;
            html.raw("<section><h2>Document paragraphs</h2>")?;
            if document.paragraphs.is_empty() {
                html.raw("<p class=\"note\">No main-body paragraphs were extracted.</p>")?;
            }
            for paragraph in &document.paragraphs {
                html.raw("<p class=\"paragraph\">")?;
                if paragraph.is_empty() {
                    html.raw("<span class=\"note\">[Empty paragraph]</span>")?;
                } else {
                    html.text(paragraph)?;
                }
                html.raw("</p>")?;
            }
            html.raw("</section>")?;
            (
                html,
                json!({"paragraphs":document.paragraphs.len()}),
                warnings,
            )
        }
        "xlsx" => {
            let workbook: Workbook = serde_json::from_value(xlsx::read(workspace, source)?)
                .map_err(|e| format!("invalid XLSX reader result: {e}"))?;
            let mut warnings = vec![
                SEMANTIC_WARNING.to_owned(),
                SPARSE_WARNING.to_owned(),
                FORMULA_WARNING.to_owned(),
            ];
            warnings.extend(workbook.warnings);
            let mut html = Html::new();
            html.start(&args.path, &warnings)?;
            let mut cell_count = 0;
            for sheet in &workbook.sheets {
                html.raw("<section><h2>Worksheet: ")?;
                html.text(&sheet.name)?;
                html.raw("</h2>")?;
                if sheet.cells.is_empty() {
                    html.raw("<p class=\"note\">No stored cells in this worksheet.</p>")?;
                } else {
                    html.raw("<div class=\"table-wrap\"><table><caption>Sparse typed cells</caption><thead><tr><th scope=\"col\">Cell</th><th scope=\"col\">Stored type</th><th scope=\"col\">Stored value</th><th scope=\"col\">Formula (not evaluated)</th></tr></thead><tbody>")?;
                    for cell in &sheet.cells {
                        html.cell(cell)?;
                    }
                    html.raw("</tbody></table></div>")?;
                }
                cell_count += sheet.cells.len();
                html.raw("</section>")?;
            }
            (
                html,
                json!({"sheets":workbook.sheets.len(),"cells":cell_count,"formula_cells":workbook.formula_cells}),
                warnings,
            )
        }
        _ => return Err("preview supports only .docx and .xlsx input paths".into()),
    };
    let bytes = html.finish()?;
    // No output is created until reading and bounded HTML generation succeed.
    // This is the same capability-confined, create-new path used by Office
    // creation tools. No subprocess, browser, or ambient path is involved.
    workspace.create(&args.output, "html", bytes.as_bytes())?;
    let mut result = json!({"path":args.output,"source":args.path,"source_format":format,"format":"html","mode":"semantic","bytes":bytes.len(),"warnings":warnings});
    result
        .as_object_mut()
        .ok_or("invalid preview result")?
        .extend(counts.as_object().ok_or("invalid preview counts")?.clone());
    Ok(result)
}

struct Html {
    output: String,
}

impl Html {
    fn new() -> Self {
        Self {
            output: String::new(),
        }
    }

    /// Only static markup and individually escaped text reach this sink.
    /// Check before each append, rather than allocating an unbounded result
    /// and only checking its size when writing the finished file.
    fn raw(&mut self, text: &str) -> Result<(), String> {
        let length = self
            .output
            .len()
            .checked_add(text.len())
            .ok_or("preview size overflow")?;
        if length > MAX_PREVIEW_BYTES {
            return Err("semantic HTML preview exceeds 16 MiB; no output was created".into());
        }
        self.output.push_str(text);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        for character in text.chars() {
            match character {
                '&' => self.raw("&amp;")?,
                '<' => self.raw("&lt;")?,
                '>' => self.raw("&gt;")?,
                '"' => self.raw("&quot;")?,
                '\'' => self.raw("&#39;")?,
                // HTML would discard NUL or normalize CR. Show these and
                // other controls explicitly instead of silently losing them.
                c if c.is_control() && !matches!(c, '\n' | '\t') => {
                    self.raw(&format!("\\u{{{:04X}}}", c as u32))?;
                }
                c => self.raw(c.encode_utf8(&mut [0; 4]))?,
            }
        }
        Ok(())
    }

    fn start(&mut self, path: &str, warnings: &[String]) -> Result<(), String> {
        self.raw("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Semantic preview: ")?;
        self.text(path)?;
        self.raw("</title><style>body{font:16px/1.5 system-ui,sans-serif;margin:0 auto;padding:2rem;max-width:80rem;color:#172033;background:#fff}h1{margin-bottom:.25rem}h2{margin-top:2rem}.notice{border:1px solid #9ca3af;background:#f3f4f6;padding:1rem}.note{color:#4b5563;font-size:.9em}.paragraph,pre{white-space:pre-wrap;overflow-wrap:anywhere;tab-size:4;unicode-bidi:plaintext}.paragraph{min-height:1.5em}pre{margin:.25rem 0;font:inherit}.table-wrap{overflow-x:auto}table{border-collapse:collapse;width:100%;table-layout:fixed}th,td{border:1px solid #9ca3af;padding:.5rem;vertical-align:top;overflow-wrap:anywhere}th{text-align:left;background:#f3f4f6}caption{text-align:left;font-weight:bold;padding:.5rem 0}th:first-child{width:6rem}th:nth-child(2){width:7rem}li{margin:.25rem 0}</style></head><body><main><header><h1>Semantic preview</h1><p>Source: <span class=\"paragraph\">")?;
        self.text(path)?;
        self.raw("</span></p></header><aside class=\"notice\" aria-label=\"Preview limitations\"><strong>Preview limitations</strong><ul>")?;
        for warning in warnings {
            self.raw("<li>")?;
            self.text(warning)?;
            self.raw("</li>")?;
        }
        self.raw("<li>Self-contained HTML: no scripts, links, remote assets, or active Office content. Control characters other than tabs and newlines are shown as Unicode escapes.</li><li>Bounded preview: at most 16 MiB of HTML; at most 2,000 paragraphs or 16 sheets and 20,000 cells, with 1,000 rows and 256 columns per sheet. Oversized input or output is rejected rather than silently truncated.</li></ul></aside>")
    }

    fn cell(&mut self, cell: &Cell) -> Result<(), String> {
        self.raw("<tr><th scope=\"row\">")?;
        self.text(&cell.cell)?;
        self.raw("</th><td>")?;
        self.text(&cell.kind)?;
        self.raw("</td><td>")?;
        if cell.formula.is_some() {
            self.raw("<span class=\"note\">Cached value (may be stale)</span>")?;
        }
        if cell.value.is_null() {
            self.raw("<p class=\"note\">No stored value</p>")?;
        } else {
            self.raw("<pre>")?;
            // Quoted strings distinguish empty strings, leading '=', numbers
            // stored as strings, and escaped control characters without
            // relying on spreadsheet number formatting or formula execution.
            self.text(&cell.value.to_string())?;
            self.raw("</pre>")?;
        }
        self.raw("</td><td>")?;
        if let Some(formula) = &cell.formula {
            if formula.text.is_empty() {
                self.raw("<p class=\"note\">No formula text stored; shared/array formulas are not expanded.</p>")?;
            } else {
                self.raw("<pre>")?;
                self.text(&formula.text)?;
                self.raw("</pre>")?;
            }
            self.raw("<p class=\"note\">Kind: ")?;
            self.text(&formula.kind)?;
            if let Some(index) = &formula.shared_index {
                self.raw("; shared index: ")?;
                self.text(index)?;
            }
            if let Some(reference) = &formula.reference {
                self.raw("; reference: ")?;
                self.text(reference)?;
            }
            self.raw("</p>")?;
        } else {
            self.raw("<span class=\"note\">No formula</span>")?;
        }
        self.raw("</td></tr>")
    }

    fn finish(mut self) -> Result<String, String> {
        self.raw("</main></body></html>")?;
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_limit_is_checked_before_each_append_including_escaping() {
        let mut html = Html::new();
        html.raw(&"x".repeat(MAX_PREVIEW_BYTES - 4)).unwrap();
        assert!(html.text("&").unwrap_err().contains("exceeds 16 MiB"));
        assert_eq!(html.output.len(), MAX_PREVIEW_BYTES - 4);
        html.raw("1234").unwrap();
        assert!(html.raw("x").is_err());
        assert_eq!(html.output.len(), MAX_PREVIEW_BYTES);
    }

    #[test]
    fn html_escape_covers_entities_quotes_unicode_and_controls() {
        let mut html = Html::new();
        html.text("<&>\"'中文😀\n\t\r\0").unwrap();
        assert_eq!(
            html.output,
            "&lt;&amp;&gt;&quot;&#39;中文😀\n\t\\u{000D}\\u{0000}"
        );
    }
}
