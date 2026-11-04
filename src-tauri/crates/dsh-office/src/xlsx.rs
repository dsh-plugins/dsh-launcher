use crate::office::{add_text, PathArgs, XlsxArgs, MAX_CELLS, MAX_COLUMNS, MAX_ROWS, MAX_SHEETS};
use crate::package::{
    escape, is_relationship, parse_xml, rels_path, root_relationship, zip_parts, Package,
    PKG_REL_NS, REL_NS, SHEET_NS, STRICT_REL_NS,
};
use crate::workspace::Workspace;
use roxmltree::Node;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) fn create(workspace: &Workspace, args: XlsxArgs) -> Result<Value, String> {
    if args.sheets.is_empty() || args.sheets.len() > MAX_SHEETS {
        return Err("XLSX requires 1..16 sheets".into());
    }
    let mut names = HashSet::new();
    let mut total = 0;
    let mut count = 0;
    let mut sheets = String::new();
    let mut relations = String::new();
    let mut overrides = String::new();
    let mut parts = Vec::new();
    for (sheet_index, sheet) in args.sheets.iter().enumerate() {
        validate_sheet_name(&sheet.name)?;
        if !names.insert(sheet.name.to_lowercase()) {
            return Err("sheet names must be unique (case-insensitive)".into());
        }
        if sheet.rows.len() > MAX_ROWS {
            return Err("sheet exceeds 1000 rows".into());
        }
        let number = sheet_index + 1;
        sheets.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{number}\" r:id=\"rId{number}\"/>",
            escape(&encode_literal(&sheet.name))?
        ));
        relations.push_str(&format!("<Relationship Id=\"rId{number}\" Type=\"{REL_NS}/worksheet\" Target=\"worksheets/sheet{number}.xml\"/>"));
        overrides.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{number}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"));
        let mut data = String::new();
        for (row_index, row) in sheet.rows.iter().enumerate() {
            if row.len() > MAX_COLUMNS {
                return Err("sheet exceeds 256 columns".into());
            }
            data.push_str(&format!("<row r=\"{}\">", row_index + 1));
            for (column, value) in row.iter().enumerate() {
                count += 1;
                if count > MAX_CELLS {
                    return Err("workbook exceeds 20000 cells".into());
                }
                let reference = cell_reference(row_index + 1, column + 1);
                match value {
                    Value::Null => {},
                    Value::Bool(value) => data.push_str(&format!("<c r=\"{reference}\" t=\"b\"><v>{}</v></c>", u8::from(*value))),
                    Value::Number(value) => {
                        // Excel keeps at most 15 significant decimal digits.
                        // Preserve identifiers and high precision as text instead.
                        let magnitude = value.as_f64().ok_or("invalid numeric cell")?.abs();
                        if magnitude > 9.99999999999999e307
                            || (magnitude != 0.0 && magnitude < f64::MIN_POSITIVE)
                        {
                            return Err("number is outside Excel's finite normal range; supply it as a string".into());
                        }
                        if value.as_i64().is_some_and(|n| n.unsigned_abs() > 999_999_999_999_999)
                            || value.as_u64().is_some_and(|n| n > 999_999_999_999_999) {
                            return Err("integer exceeds Excel 15-digit numeric range; supply it as a string".into());
                        }
                        let numeric_text = value.to_string();
                        let mantissa = numeric_text.split(['e', 'E']).next().unwrap_or("");
                        let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
                        if digits.trim_start_matches('0').len() > 15 {
                            return Err("number exceeds Excel 15-digit precision; supply it as a string".into());
                        }
                        data.push_str(&format!("<c r=\"{reference}\"><v>{value}</v></c>"));
                    }
                    Value::String(value) => {
                        validate_cell_string(value)?;
                        add_text(&mut total, value)?;
                        // inlineStr prevents formula injection, even for text
                        // beginning '=', '+', '-', '@' or DDE-like content.
                        data.push_str(&format!("<c r=\"{reference}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", escape(&encode_literal(value))?));
                    }
                    _ => return Err("cells must be strings, numbers, booleans or null; formula objects are unsupported".into()),
                }
            }
            data.push_str("</row>");
        }
        parts.push((format!("xl/worksheets/sheet{number}.xml"), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="{SHEET_NS}"><sheetData>{data}</sheetData></worksheet>"#)));
    }
    parts.push(("xl/workbook.xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="{SHEET_NS}" xmlns:r="{REL_NS}"><sheets>{sheets}</sheets></workbook>"#)));
    parts.push(("xl/_rels/workbook.xml.rels".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG_REL_NS}">{relations}</Relationships>"#)));
    parts.push(("_rels/.rels".into(), root_relationship("xl/workbook.xml")));
    parts.push(("[Content_Types].xml".into(), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#)));
    let bytes = zip_parts(parts)?;
    workspace.create(&args.path, "xlsx", &bytes)?;
    Ok(
        json!({"path":args.path,"format":"xlsx","sheets":args.sheets.len(),"cells":count,"bytes":bytes.len()}),
    )
}

pub(crate) fn read(workspace: &Workspace, args: PathArgs) -> Result<Value, String> {
    let mut package = Package::open(workspace.read(&args.path, "xlsx")?)?;
    let main = package.main_part()?;
    let workbook_xml = package.xml(&main)?;
    let workbook = parse_xml(&workbook_xml)?;
    let root = workbook.root_element();
    let namespace = root
        .tag_name()
        .namespace()
        .ok_or("missing XLSX namespace")?;
    if root.tag_name().name() != "workbook"
        || ![SHEET_NS, "http://purl.oclc.org/ooxml/spreadsheetml/main"].contains(&namespace)
    {
        return Err("package is not a supported XLSX workbook".into());
    }
    let relationships = package.relationships(&rels_path(&main))?;
    let mut shared = Vec::new();
    let mut total = 0;
    let shared_parts: Vec<_> = relationships
        .iter()
        .filter(|r| is_relationship(&r.kind, "sharedStrings"))
        .collect();
    if shared_parts.len() > 1 {
        return Err("multiple shared string tables".into());
    }
    if let Some(relationship) = shared_parts.first() {
        let xml = package.xml(&relationship.target)?;
        let document = parse_xml(&xml)?;
        if !document.root_element().has_tag_name((namespace, "sst")) {
            return Err("invalid shared string table".into());
        }
        for item in document
            .root_element()
            .children()
            .filter(|n| n.has_tag_name((namespace, "si")))
        {
            if shared.len() >= MAX_CELLS {
                return Err("shared string table exceeds 20000 entries".into());
            }
            let text = rich_text(item, namespace)?;
            add_text(&mut total, &text)?;
            shared.push(text);
        }
    }
    let sheets = root
        .children()
        .find(|n| n.has_tag_name((namespace, "sheets")))
        .ok_or("XLSX sheets are missing")?;
    let mut results = Vec::new();
    let mut names = HashSet::new();
    let mut targets = HashSet::new();
    let mut count = 0;
    let mut formulas = 0;
    for sheet in sheets
        .children()
        .filter(|n| n.has_tag_name((namespace, "sheet")))
    {
        if results.len() >= MAX_SHEETS {
            return Err("workbook exceeds 16 sheets".into());
        }
        let name = decode_literal(sheet.attribute("name").ok_or("sheet name is missing")?)?;
        validate_sheet_name(&name)?;
        if !names.insert(name.to_lowercase()) {
            return Err("duplicate sheet name".into());
        }
        let id = sheet
            .attribute((REL_NS, "id"))
            .or_else(|| sheet.attribute((STRICT_REL_NS, "id")))
            .ok_or("sheet relationship is missing")?;
        let relationship = relationships
            .iter()
            .find(|r| r.id == id)
            .ok_or("sheet relationship does not exist")?;
        if !is_relationship(&relationship.kind, "worksheet") {
            return Err("only ordinary worksheets are supported".into());
        }
        if !targets.insert(&relationship.target) {
            return Err("duplicate worksheet target".into());
        }
        let xml = package.xml(&relationship.target)?;
        let document = parse_xml(&xml)?;
        if !document
            .root_element()
            .has_tag_name((namespace, "worksheet"))
        {
            return Err("invalid worksheet part".into());
        }
        let data = document
            .root_element()
            .children()
            .find(|n| n.has_tag_name((namespace, "sheetData")))
            .ok_or("worksheet sheetData is missing")?;
        let mut cells = Vec::new();
        let mut row_index = 0;
        let mut seen_rows = HashSet::new();
        for row in data
            .children()
            .filter(|n| n.has_tag_name((namespace, "row")))
        {
            row_index = match row.attribute("r") {
                Some(s) => s.parse::<usize>().map_err(|_| "invalid row number")?,
                None => row_index + 1,
            };
            if row_index == 0 || row_index > MAX_ROWS || !seen_rows.insert(row_index) {
                return Err("invalid, repeated or out-of-bounds row (limit 1000)".into());
            }
            let mut column = 0;
            let mut seen_columns = HashSet::new();
            for cell in row.children().filter(|n| n.has_tag_name((namespace, "c"))) {
                count += 1;
                if count > MAX_CELLS {
                    return Err("workbook exceeds 20000 cells".into());
                }
                let (r, c) = match cell.attribute("r") {
                    Some(s) => parse_reference(s)?,
                    None => (row_index, column + 1),
                };
                if r != row_index || c == 0 || c > MAX_COLUMNS || !seen_columns.insert(c) {
                    return Err(
                        "invalid, repeated or out-of-bounds cell (limit 256 columns)".into(),
                    );
                }
                column = c;
                let raw = child_text(cell, namespace, "v").unwrap_or("");
                let cell_type = cell.attribute("t").unwrap_or("n");
                let (kind, value) = match cell_type {
                    "inlineStr" => {
                        let inline = cell
                            .children()
                            .find(|n| n.has_tag_name((namespace, "is")))
                            .ok_or("inline string is missing")?;
                        ("string", Value::String(rich_text(inline, namespace)?))
                    }
                    "s" => {
                        let index: usize =
                            raw.parse().map_err(|_| "invalid shared string index")?;
                        (
                            "string",
                            Value::String(
                                shared
                                    .get(index)
                                    .ok_or("shared string index is out of range")?
                                    .clone(),
                            ),
                        )
                    }
                    "str" => ("string", Value::String(decode_literal(raw)?)),
                    "b" => (
                        "boolean",
                        match raw.trim() {
                            "0" | "false" => json!(false),
                            "1" | "true" => json!(true),
                            "" => Value::Null,
                            _ => return Err("invalid boolean cell".into()),
                        },
                    ),
                    "n" => (
                        "number",
                        if raw.trim().is_empty() {
                            Value::Null
                        } else {
                            parse_number(raw)?
                        },
                    ),
                    "e" => ("error", Value::String(raw.to_owned())),
                    "d" => ("date", Value::String(raw.to_owned())),
                    _ => return Err("unsupported XLSX cell type".into()),
                };
                if let Some(text) = value.as_str() {
                    validate_cell_string(text)?;
                    add_text(&mut total, text)?;
                }
                let mut output = json!({"cell":cell_reference(r,c),"type":kind,"value":value});
                if let Some(formula) = cell.children().find(|n| n.has_tag_name((namespace, "f"))) {
                    formulas += 1;
                    let text = formula.text().unwrap_or("");
                    add_text(&mut total, text)?;
                    output["formula"] = json!({"text":text,"kind":formula.attribute("t").unwrap_or("normal"),
                        "shared_index":formula.attribute("si"),"reference":formula.attribute("ref"),"evaluated":false});
                }
                cells.push(output);
            }
        }
        results.push(json!({"name":name,"cells":cells}));
    }
    if results.is_empty() {
        return Err("workbook has no worksheets".into());
    }
    Ok(
        json!({"path":args.path,"format":"xlsx","sheets":results,"formula_cells":formulas,
        "warnings":["Sparse cells only. Formatting, charts, merges, hidden state and date styles are not reconstructed.","Formulas are not evaluated; cached values may be absent or stale. Shared/array formulas are not expanded. Numeric dates are returned as raw serials."]}),
    )
}

fn validate_sheet_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.encode_utf16().count() > 31
        || name.contains(['[', ']', ':', '*', '?', '/', '\\'])
        || name.starts_with('\'')
        || name.ends_with('\'')
        || name.chars().any(char::is_control)
    {
        return Err("invalid Excel sheet name (1..31 UTF-16 units; no []:*?/\\ or leading/trailing apostrophe)".into());
    }
    Ok(())
}

fn cell_reference(row: usize, mut column: usize) -> String {
    let mut letters = Vec::new();
    while column > 0 {
        column -= 1;
        letters.push((b'A' + (column % 26) as u8) as char);
        column /= 26;
    }
    letters.into_iter().rev().collect::<String>() + &row.to_string()
}

fn parse_reference(reference: &str) -> Result<(usize, usize), String> {
    if reference.len() > 10 {
        return Err("cell reference is too long".into());
    }
    let letters = reference.bytes().take_while(u8::is_ascii_uppercase).count();
    if letters == 0 || letters > 3 || letters == reference.len() {
        return Err("invalid A1 cell reference".into());
    }
    let mut column = 0usize;
    for c in reference[..letters].bytes() {
        column = column * 26 + usize::from(c - b'A' + 1);
    }
    let row_text = &reference[letters..];
    if row_text.starts_with('0') || !row_text.bytes().all(|c| c.is_ascii_digit()) {
        return Err("invalid A1 row".into());
    }
    let row = row_text.parse::<usize>().map_err(|_| "invalid A1 row")?;
    if row == 0 || row > MAX_ROWS || column > MAX_COLUMNS {
        return Err("cell is outside the 1000-row/256-column limit".into());
    }
    Ok((row, column))
}

fn child_text<'a, 'input>(node: Node<'a, 'input>, namespace: &str, tag: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.has_tag_name((namespace, tag)))
        .and_then(|n| n.text())
}

fn rich_text(node: Node<'_, '_>, namespace: &str) -> Result<String, String> {
    let mut result = String::new();
    for text in node
        .descendants()
        .filter(|n| n.has_tag_name((namespace, "t")))
    {
        if text.ancestors().any(|n| n.has_tag_name((namespace, "rPh"))) {
            continue;
        }
        result.push_str(text.text().unwrap_or(""));
    }
    decode_literal(&result)
}

fn parse_number(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value.is_number() {
            return Ok(value);
        }
    }
    let value: f64 = text.parse().map_err(|_| "invalid numeric cell")?;
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| "non-finite numeric cells are unsupported".into())
}

// OOXML ST_Xstring uses _xHHHH_ escapes. Escape an input literal underscore
// only when it would otherwise begin such a sequence, including _x005F_.
fn encode_literal(text: &str) -> String {
    let mut out = String::new();
    for (offset, c) in text.char_indices() {
        if c == '_' && escaped_unit(text.as_bytes(), offset).is_some() {
            out.push_str("_x005F_");
        } else {
            out.push(c);
        }
    }
    out
}
fn escaped_unit(bytes: &[u8], offset: usize) -> Option<u16> {
    let part = bytes.get(offset..offset + 7)?;
    if part[0] != b'_' || part[1] != b'x' || part[6] != b'_' {
        return None;
    }
    let digits = std::str::from_utf8(&part[2..6]).ok()?;
    u16::from_str_radix(digits, 16).ok()
}
fn decode_literal(text: &str) -> Result<String, String> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        if let Some(unit) = escaped_unit(text.as_bytes(), offset) {
            result.push(unit);
            offset += 7;
        } else {
            let c = text[offset..].chars().next().ok_or("invalid text offset")?;
            let mut buffer = [0; 2];
            result.extend_from_slice(c.encode_utf16(&mut buffer));
            offset += c.len_utf8();
        }
    }
    String::from_utf16(&result).map_err(|_| "invalid UTF-16 Office text escape".into())
}

fn validate_cell_string(text: &str) -> Result<(), String> {
    if text.encode_utf16().count() > 32_767 {
        return Err("Excel cell text exceeds 32767 UTF-16 units".into());
    }
    Ok(())
}
