use dsh_office::{serve, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};
use serde_json::{json, Value};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "dsh-office-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
}
fn ready() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}
fn call(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}
fn exchange(workspace: &Workspace, messages: Vec<Value>) -> Vec<Value> {
    let input = messages
        .into_iter()
        .map(|v| v.to_string() + "\n")
        .collect::<String>();
    let mut output = Vec::new();
    serve(&workspace.0, Cursor::new(input), &mut output).unwrap();
    output
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| {
            assert!(s.len() <= MAX_RESPONSE_BYTES);
            serde_json::from_slice(s).unwrap()
        })
        .collect()
}
fn tool(workspace: &Workspace, name: &str, arguments: Value) -> Value {
    exchange(
        workspace,
        vec![initialize(), ready(), call(name, arguments)],
    )
    .pop()
    .unwrap()
}
fn data(response: &Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}
fn fails(response: Value, fragment: &str) {
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(fragment),
        "{response}"
    );
}
fn make_doc(workspace: &Workspace, path: &str) {
    data(&tool(
        workspace,
        "docx_create",
        json!({"path":path,"paragraphs":["Hello"]}),
    ));
}
fn make_book(workspace: &Workspace, path: &str) {
    data(&tool(
        workspace,
        "xlsx_create",
        json!({"path":path,"sheets":[{"name":"Data","rows":[["Hello",1,true,null]]}]}),
    ));
}

// Mutate fixtures through ZIP APIs. No external Office, scripting runtime,
// shell command or network access is involved in these regression tests.
fn rewrite(workspace: &Workspace, path: &str, mut edit: impl FnMut(&str, &mut String)) {
    let input = fs::read(workspace.0.join(path)).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(input)).unwrap();
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..zip.len() {
        let mut part = zip.by_index(index).unwrap();
        let name = part.name().to_owned();
        let mut text = String::new();
        part.read_to_string(&mut text).unwrap();
        edit(&name, &mut text);
        out.start_file(name, SimpleFileOptions::default()).unwrap();
        out.write_all(text.as_bytes()).unwrap();
    }
    fs::write(workspace.0.join(path), out.finish().unwrap().into_inner()).unwrap();
}

#[test]
fn lifecycle_and_tool_schema() {
    let workspace = Workspace::new();
    let messages = exchange(
        &workspace,
        vec![
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
            initialize(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            ready(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":"p","method":"ping"}),
            json!({"jsonrpc":"2.0","id":5,"method":"missing"}),
        ],
    );
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[0]["error"]["code"], -32002);
    assert_eq!(messages[1]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(messages[2]["error"]["code"], -32002);
    assert_eq!(messages[3]["result"]["tools"].as_array().unwrap().len(), 5);
    assert_eq!(messages[4]["id"], "p");
    assert_eq!(messages[4]["result"], json!({}));
    assert_eq!(messages[5]["error"]["code"], -32601);
}

#[test]
fn docx_round_trip_preserves_unicode_entities_and_controls() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.0.join("nested")).unwrap();
    let paragraphs = json!([
        "中文 & <tag> \"quoted\" 'apostrophe' 😀",
        "  spaced  \nline\ttab\rcarriage",
        "",
        "_x0041_"
    ]);
    data(&tool(
        &workspace,
        "docx_create",
        json!({"path":"nested/report.docx","paragraphs":paragraphs}),
    ));
    let result = data(&tool(
        &workspace,
        "docx_read",
        json!({"path":"nested/report.docx"}),
    ));
    assert_eq!(result["paragraphs"], paragraphs);
}

#[test]
fn xlsx_round_trip_preserves_types_and_literal_formula_text() {
    let workspace = Workspace::new();
    let values = json!([
        "中文 & <>",
        42,
        -1.5,
        true,
        false,
        null,
        "=SUM(A1:A2)",
        "_x0041_",
        "_x005F_x0041_",
        "😀\r\n"
    ]);
    data(&tool(
        &workspace,
        "xlsx_create",
        json!({"path":"book.xlsx","sheets":[{"name":"Data","rows":[values]},{"name":"Empty","rows":[]}]}),
    ));
    let result = data(&tool(&workspace, "xlsx_read", json!({"path":"book.xlsx"})));
    let cells = result["sheets"][0]["cells"].as_array().unwrap();
    let expected: Vec<Value> = values
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| !v.is_null())
        .cloned()
        .collect();
    assert_eq!(
        cells.iter().map(|c| c["value"].clone()).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(result["formula_cells"], 0);
    assert_eq!(cells[5]["cell"], "G1");
    assert_eq!(cells[5]["type"], "string");
}

#[test]
fn never_overwrites_and_rejects_unknown_create_flags() {
    let workspace = Workspace::new();
    make_doc(&workspace, "same.docx");
    let before = fs::read(workspace.0.join("same.docx")).unwrap();
    fails(
        tool(
            &workspace,
            "docx_create",
            json!({"path":"same.docx","paragraphs":["changed"]}),
        ),
        "overwriting",
    );
    assert_eq!(before, fs::read(workspace.0.join("same.docx")).unwrap());
    fails(
        tool(
            &workspace,
            "docx_create",
            json!({"path":"other.docx","paragraphs":["changed"],"overwrite":true}),
        ),
        "unknown field",
    );
}

#[test]
fn traversal_absolute_windows_ads_and_ambiguous_paths_fail() {
    let workspace = Workspace::new();
    for path in [
        "../escape.docx",
        "/tmp/escape.docx",
        "a/../escape.docx",
        "a//b.docx",
        "./escape.docx",
        "C:\\escape.docx",
        "a:stream.docx",
        "NUL.docx",
        "a./b.docx",
        "a\\b.docx",
    ] {
        assert_eq!(
            tool(
                &workspace,
                "docx_create",
                json!({"path":path,"paragraphs":["no"]})
            )["result"]["isError"],
            true,
            "{path}"
        );
    }
    assert!(serve(std::path::Path::new("."), Cursor::new(""), Vec::new()).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_files_and_parent_directories_are_rejected() {
    use std::os::unix::fs::symlink;
    let workspace = Workspace::new();
    let outside = Workspace::new();
    make_doc(&outside, "secret.docx");
    symlink(outside.0.join("secret.docx"), workspace.0.join("link.docx")).unwrap();
    symlink(&outside.0, workspace.0.join("linked")).unwrap();
    fails(
        tool(&workspace, "docx_read", json!({"path":"link.docx"})),
        "symlink",
    );
    fails(
        tool(
            &workspace,
            "docx_create",
            json!({"path":"linked/new.docx","paragraphs":["no"]}),
        ),
        "symlink",
    );
    assert!(!outside.0.join("new.docx").exists());
}

#[test]
fn bounded_inputs_and_invalid_xml_text_are_rejected() {
    let workspace = Workspace::new();
    fails(
        tool(
            &workspace,
            "docx_create",
            json!({"path":"large.docx","paragraphs":["x".repeat(32769)]}),
        ),
        "32 KiB",
    );
    fails(
        tool(
            &workspace,
            "docx_create",
            json!({"path":"bad.docx","paragraphs":["bad\u{0}"]}),
        ),
        "XML 1.0",
    );
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"bad.xlsx","sheets":[{"name":"Bad/Name","rows":[]}]}),
        ),
        "sheet name",
    );
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"bad.xlsx","sheets":[{"name":"Data","rows":[[{"formula":"1+1"}]]}]}),
        ),
        "formula objects",
    );
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"bad.xlsx","sheets":[{"name":"Data","rows":[[9007199254740992u64]]}]}),
        ),
        "15-digit numeric range",
    );
    let mut output = Vec::new();
    let error = serve(
        &workspace.0,
        Cursor::new(vec![b'x'; MAX_REQUEST_BYTES + 1]),
        &mut output,
    )
    .unwrap_err();
    assert!(error.contains("request exceeds"));
    assert!(output.is_empty());
}

#[test]
fn malformed_json_recovers_but_batch_requests_are_rejected() {
    let workspace = Workspace::new();
    let input = b"not-json\n[]\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
    let mut output = Vec::new();
    serve(&workspace.0, Cursor::new(input), &mut output).unwrap();
    let replies: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["error"]["code"], -32600);
    assert_eq!(replies[2]["result"], json!({}));
}

#[test]
fn rejects_dtd_external_relationship_and_macro_content() {
    let workspace = Workspace::new();
    make_doc(&workspace, "dtd.docx");
    rewrite(&workspace, "dtd.docx", |name, text| {
        if name == "word/document.xml" {
            *text = text.replace(
                "<w:document",
                "<!DOCTYPE x [<!ENTITY xx SYSTEM 'file:///etc/passwd'>]><w:document",
            );
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"dtd.docx"})),
        "XML",
    );
    make_doc(&workspace, "external.docx");
    rewrite(&workspace, "external.docx", |name, text| {
        if name == "_rels/.rels" {
            *text = text.replace("Target=", "TargetMode=\"External\" Target=");
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"external.docx"})),
        "external relationships",
    );
    make_doc(&workspace, "macro.docx");
    rewrite(&workspace, "macro.docx", |name, text| {
        if name == "[Content_Types].xml" {
            *text = text.replace(
                "wordprocessingml.document.main+xml",
                "word.macroEnabled.main+xml",
            );
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"macro.docx"})),
        "active Office",
    );
}

#[test]
fn formula_cached_values_are_reported_without_evaluation() {
    let workspace = Workspace::new();
    make_book(&workspace, "formula.xlsx");
    rewrite(&workspace, "formula.xlsx", |name, text| {
        if name == "xl/worksheets/sheet1.xml" {
            *text = text.replace(
                "<c r=\"B1\"><v>1</v></c>",
                "<c r=\"B1\"><f>999+1</f><v>7</v></c>",
            );
        }
    });
    let result = data(&tool(
        &workspace,
        "xlsx_read",
        json!({"path":"formula.xlsx"}),
    ));
    assert_eq!(result["formula_cells"], 1);
    assert_eq!(result["sheets"][0]["cells"][1]["formula"]["text"], "999+1");
    assert_eq!(
        result["sheets"][0]["cells"][1]["formula"]["evaluated"],
        false
    );
    assert_eq!(result["sheets"][0]["cells"][1]["value"], 7);
}

#[test]
fn hostile_zip_paths_and_size_limits_fail_before_parsing() {
    let workspace = Workspace::new();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("../escaped.xml", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<xml/>").unwrap();
    fs::write(
        workspace.0.join("bad.docx"),
        zip.finish().unwrap().into_inner(),
    )
    .unwrap();
    fails(
        tool(&workspace, "docx_read", json!({"path":"bad.docx"})),
        "unsafe",
    );
    let file = fs::File::create(workspace.0.join("huge.docx")).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    fails(
        tool(&workspace, "docx_read", json!({"path":"huge.docx"})),
        "16 MiB",
    );
}

#[test]
fn sparse_out_of_bounds_cells_are_rejected_without_dense_allocation() {
    let workspace = Workspace::new();
    make_book(&workspace, "sparse.xlsx");
    rewrite(&workspace, "sparse.xlsx", |name, text| {
        if name == "xl/worksheets/sheet1.xml" {
            *text = text.replace("r=\"A1\"", "r=\"XFD1048576\"");
        }
    });
    fails(
        tool(&workspace, "xlsx_read", json!({"path":"sparse.xlsx"})),
        "limit",
    );
}

#[test]
fn excel_sheet_names_preserve_st_xstring_literals() {
    let workspace = Workspace::new();
    for (index, name) in ["_x0041_", "中文😀", "_x005F_x0041_"].iter().enumerate() {
        let path = format!("names{index}.xlsx");
        data(&tool(
            &workspace,
            "xlsx_create",
            json!({"path":path,"sheets":[{"name":name,"rows":[]}]}),
        ));
        let result = data(&tool(&workspace, "xlsx_read", json!({"path":path})));
        assert_eq!(result["sheets"][0]["name"], *name);
    }
}

#[test]
fn reads_shared_rich_strings_and_ignores_phonetic_annotations() {
    let workspace = Workspace::new();
    make_book(&workspace, "shared.xlsx");
    rewrite(&workspace, "shared.xlsx", |name, text| {
        if name == "xl/_rels/workbook.xml.rels" {
            *text = text.replace("</Relationships>", "<Relationship Id=\"strings\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/></Relationships>");
        }
        if name == "xl/worksheets/sheet1.xml" {
            *text = text.replace(
                "<c r=\"A1\" t=\"inlineStr\"><is><t xml:space=\"preserve\">Hello</t></is></c>",
                "<c r=\"A1\" t=\"s\"><v>0</v></c>",
            );
        }
    });
    append_part(
        &workspace,
        "shared.xlsx",
        "xl/sharedStrings.xml",
        r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><r><t>Hi &amp; </t></r><r><t>世界</t></r><rPh><t>ignored</t></rPh></si></sst>"#,
    );
    let result = data(&tool(
        &workspace,
        "xlsx_read",
        json!({"path":"shared.xlsx"}),
    ));
    assert_eq!(result["sheets"][0]["cells"][0]["value"], "Hi & 世界");
}

fn append_part(workspace: &Workspace, path: &str, name: &str, content: &str) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.0.join(path))
        .unwrap();
    let mut zip = ZipWriter::new_append(file).unwrap();
    zip.start_file(name, SimpleFileOptions::default()).unwrap();
    zip.write_all(content.as_bytes()).unwrap();
    zip.finish().unwrap();
}

#[test]
fn reads_strict_ooxml_namespaces() {
    let workspace = Workspace::new();
    make_doc(&workspace, "strict.docx");
    make_book(&workspace, "strict.xlsx");
    for path in ["strict.docx", "strict.xlsx"] {
        rewrite(&workspace, path, |_, text| {
            *text = text
                .replace(
                    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                    "http://purl.oclc.org/ooxml/wordprocessingml/main",
                )
                .replace(
                    "http://schemas.openxmlformats.org/spreadsheetml/2006/main",
                    "http://purl.oclc.org/ooxml/spreadsheetml/main",
                )
                .replace(
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
                    "http://purl.oclc.org/ooxml/officeDocument/relationships",
                );
        });
    }
    assert_eq!(
        data(&tool(
            &workspace,
            "docx_read",
            json!({"path":"strict.docx"})
        ))["paragraphs"][0],
        "Hello"
    );
    assert_eq!(
        data(&tool(
            &workspace,
            "xlsx_read",
            json!({"path":"strict.xlsx"})
        ))["sheets"][0]["cells"][0]["value"],
        "Hello"
    );
}

#[test]
fn xml_expansion_and_nesting_are_bounded() {
    let workspace = Workspace::new();
    make_doc(&workspace, "large-xml.docx");
    rewrite(&workspace, "large-xml.docx", |name, text| {
        if name == "word/document.xml" {
            *text = "x".repeat(2 * 1024 * 1024 + 1);
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"large-xml.docx"})),
        "XML part exceeds",
    );
    make_doc(&workspace, "deep.docx");
    rewrite(&workspace, "deep.docx", |name, text| {
        if name == "word/document.xml" {
            *text = "<x>".repeat(200) + &"</x>".repeat(200);
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"deep.docx"})),
        "nesting exceeds",
    );
}

#[test]
fn excel_cell_length_and_decimal_precision_are_enforced() {
    let workspace = Workspace::new();
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"too-long.xlsx","sheets":[{"name":"Data","rows":[["a".repeat(32768)]]}]}),
        ),
        "32767",
    );
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"too-precise.xlsx","sheets":[{"name":"Data","rows":[[1.2345678901234567]]}]}),
        ),
        "15-digit precision",
    );
    fails(
        tool(
            &workspace,
            "xlsx_create",
            json!({"path":"too-many-digits.xlsx","sheets":[{"name":"Data","rows":[[1234567890123456u64]]}]}),
        ),
        "15-digit numeric range",
    );
    data(&tool(
        &workspace,
        "xlsx_create",
        json!({"path":"boundary.xlsx","sheets":[{"name":"Data","rows":[["a".repeat(32767),999999999999999u64]]}]}),
    ));
}

#[test]
fn response_limit_is_an_explicit_error_not_truncated_json() {
    let workspace = Workspace::new();
    // Null/blank output uses no text budget, but 20000 sparse cell records
    // still exceed the protocol's 1 MiB response budget.
    make_book(&workspace, "response.xlsx");
    rewrite(&workspace, "response.xlsx", |name, text| {
        if name == "xl/worksheets/sheet1.xml" {
            let mut rows = String::new();
            for row in 1..=1000 {
                rows.push_str(&format!("<row r=\"{row}\">"));
                for column in b'A'..=b'T' {
                    rows.push_str(&format!(
                        "<c r=\"{}{row}\" t=\"str\"><v>1234567890</v></c>",
                        column as char
                    ));
                }
                rows.push_str("</row>");
            }
            *text = format!("<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{rows}</sheetData></worksheet>");
        }
    });
    let response = tool(&workspace, "xlsx_read", json!({"path":"response.xlsx"}));
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("response limit"));
}

#[test]
fn xml_attribute_and_namespace_counts_are_bounded_before_tree_parse() {
    let workspace = Workspace::new();
    make_doc(&workspace, "attributes.docx");
    rewrite(&workspace, "attributes.docx", |name, text| {
        if name == "word/document.xml" {
            let attributes = (0..20000)
                .map(|n| format!(" a{n}=\"\""))
                .collect::<String>();
            *text = format!("<x{attributes}/>");
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"attributes.docx"})),
        "64 attributes",
    );
    make_doc(&workspace, "namespaces.docx");
    rewrite(&workspace, "namespaces.docx", |name, text| {
        if name == "word/document.xml" {
            let children = (0..200)
                .map(|n| format!("<x xmlns:p{n}=\"urn:{n}\"/>"))
                .collect::<String>();
            *text = format!("<root>{children}</root>");
        }
    });
    fails(
        tool(&workspace, "docx_read", json!({"path":"namespaces.docx"})),
        "128 namespace",
    );
}

#[test]
fn excel_numeric_range_rejects_overflow_and_subnormals() {
    let workspace = Workspace::new();
    for number in [1e308, 1e-320, -1e308, -1e-320] {
        fails(
            tool(
                &workspace,
                "xlsx_create",
                json!({"path":"range.xlsx","sheets":[{"name":"Data","rows":[[number]]}]}),
            ),
            "finite normal range",
        );
    }
}
