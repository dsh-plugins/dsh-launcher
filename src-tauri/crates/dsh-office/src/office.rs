use crate::{docx, workspace::Workspace, xlsx};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_STRING_BYTES: usize = 32 * 1024;
pub(crate) const MAX_PARAGRAPHS: usize = 2000;
pub(crate) const MAX_SHEETS: usize = 16;
pub(crate) const MAX_ROWS: usize = 1000;
pub(crate) const MAX_COLUMNS: usize = 256;
pub(crate) const MAX_CELLS: usize = 20_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathArgs {
    pub path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocxArgs {
    pub path: String,
    pub paragraphs: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct XlsxArgs {
    pub path: String,
    pub sheets: Vec<SheetInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SheetInput {
    pub name: String,
    pub rows: Vec<Vec<Value>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

pub(crate) fn has_tool(name: &str) -> bool {
    matches!(
        name,
        "office_info" | "docx_create" | "docx_read" | "xlsx_create" | "xlsx_read"
    )
}

pub(crate) fn call(workspace: &Workspace, name: &str, arguments: Value) -> Result<Value, String> {
    match name {
        "office_info" => {
            let _: EmptyArgs =
                serde_json::from_value(arguments).map_err(|e| format!("invalid arguments: {e}"))?;
            Ok(
                json!({"runtime":"rust","formats":["docx","xlsx"],"create_policy":"new files only; existing parent directories required",
                "path_policy":"relative paths under the explicitly selected workspace; no symlink paths",
                "unsupported":["PPTX","formula evaluation","rendering/PDF export","full-fidelity editing","macros","embedded objects","external relationships","encrypted files","non-UTF-8 XML"],
                "limits":{"request_bytes":crate::MAX_REQUEST_BYTES,"response_bytes":crate::MAX_RESPONSE_BYTES,"file_bytes":crate::workspace::MAX_FILE_BYTES,
                    "zip_entries":256,"expanded_zip_bytes":32*1024*1024,"xml_part_bytes":crate::package::MAX_XML_BYTES,"xml_nodes_per_part":100000,"xml_nesting":128,"xml_attributes_per_element":64,"xml_namespace_declarations_per_part":128,"excel_cell_utf16_units":32767,"excel_numeric_significant_digits":15,
                    "text_bytes":MAX_TEXT_BYTES,"string_bytes":MAX_STRING_BYTES,"paragraphs":MAX_PARAGRAPHS,"sheets":MAX_SHEETS,"rows_per_sheet":MAX_ROWS,"columns_per_sheet":MAX_COLUMNS,"cells_per_workbook":MAX_CELLS}}),
            )
        }
        "docx_create" => docx::create(workspace, decode(arguments)?),
        "docx_read" => docx::read(workspace, decode(arguments)?),
        "xlsx_create" => xlsx::create(workspace, decode(arguments)?),
        "xlsx_read" => xlsx::read(workspace, decode(arguments)?),
        _ => Err("unknown tool".into()),
    }
}
fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("invalid arguments: {e}"))
}

pub(crate) fn add_text(total: &mut usize, text: &str) -> Result<(), String> {
    if text.len() > MAX_STRING_BYTES {
        return Err("a text value exceeds 32 KiB".into());
    }
    *total = total.checked_add(text.len()).ok_or("text size overflow")?;
    if *total > MAX_TEXT_BYTES {
        return Err("document text exceeds 256 KiB".into());
    }
    Ok(())
}

pub(crate) fn tool_definitions() -> Vec<Value> {
    let path = json!({"type":"string","minLength":1,"maxLength":1024,"description":"Workspace-relative path; parent directories must exist."});
    let read_schema = json!({"type":"object","properties":{"path":path},"required":["path"],"additionalProperties":false});
    vec![
        tool("office_info", "Describe supported Rust-only Office operations, hard limits, and unsupported features.", json!({"type":"object","properties":{},"additionalProperties":false}), true),
        tool("docx_create", "Create a new DOCX from plain paragraph text. Never overwrites. No formatting or active content.", json!({"type":"object","properties":{"path":path,"paragraphs":{"type":"array","minItems":1,"maxItems":MAX_PARAGRAPHS,"items":{"type":"string","maxLength":MAX_STRING_BYTES}}},"required":["path","paragraphs"],"additionalProperties":false}), false),
        tool("docx_read", "Extract main-body paragraph text from bounded UTF-8 DOCX. Formatting is not preserved; external relationships are rejected.", read_schema.clone(), true),
        tool("xlsx_create", "Create a new XLSX with named sheets and string/number/boolean/null cells (numbers limited to 15 significant decimal digits). Strings, including '=' prefixes, remain literal text. No formulas or overwriting.", json!({"type":"object","properties":{"path":path,"sheets":{"type":"array","minItems":1,"maxItems":MAX_SHEETS,"items":{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":31},"rows":{"type":"array","maxItems":MAX_ROWS,"items":{"type":"array","maxItems":MAX_COLUMNS,"items":{"type":["string","number","boolean","null"]}}}},"required":["name","rows"],"additionalProperties":false}}},"required":["path","sheets"],"additionalProperties":false}), false),
        tool("xlsx_read", "Extract sparse typed cells from bounded UTF-8 XLSX. Formula text and possibly stale cached values are reported, never evaluated. Dates are raw numeric serials unless ISO-8601 cells.", read_schema, true),
    ]
}
fn tool(name: &str, description: &str, input_schema: Value, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema,
        "annotations":{"readOnlyHint":read_only,"destructiveHint":false,"idempotentHint":read_only,"openWorldHint":false}})
}
