use crate::office::{add_text, DocxArgs, PathArgs, MAX_PARAGRAPHS};
use crate::package::{escape, parse_xml, root_relationship, zip_parts, Package, WORD_NS};
use crate::workspace::Workspace;
use serde_json::{json, Value};

pub(crate) fn create(workspace: &Workspace, args: DocxArgs) -> Result<Value, String> {
    if args.paragraphs.is_empty() || args.paragraphs.len() > MAX_PARAGRAPHS {
        return Err("DOCX requires 1..2000 paragraphs".into());
    }
    let mut total = 0;
    let mut body = String::new();
    for paragraph in &args.paragraphs {
        add_text(&mut total, paragraph)?;
        body.push_str("<w:p><w:r>");
        // Use actual Word break/tab elements rather than relying on how an
        // Office client displays literal newlines embedded in text nodes.
        for (index, line) in paragraph.split('\n').enumerate() {
            if index != 0 {
                body.push_str("<w:br/>");
            }
            for (index, chunk) in line.split('\t').enumerate() {
                if index != 0 {
                    body.push_str("<w:tab/>");
                }
                body.push_str(&format!(
                    "<w:t xml:space=\"preserve\">{}</w:t>",
                    escape(chunk)?
                ));
            }
        }
        body.push_str("</w:r></w:p>");
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{WORD_NS}"><w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#
    );
    let types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let bytes = zip_parts(vec![
        ("[Content_Types].xml".into(), types.into()),
        ("_rels/.rels".into(), root_relationship("word/document.xml")),
        ("word/document.xml".into(), document),
    ])?;
    workspace.create(&args.path, "docx", &bytes)?;
    Ok(
        json!({"path":args.path,"format":"docx","paragraphs":args.paragraphs.len(),"bytes":bytes.len()}),
    )
}

pub(crate) fn read(workspace: &Workspace, args: PathArgs) -> Result<Value, String> {
    let mut package = Package::open(workspace.read(&args.path, "docx")?)?;
    let main = package.main_part()?;
    let xml = package.xml(&main)?;
    let document = parse_xml(&xml)?;
    let root = document.root_element();
    let namespace = root
        .tag_name()
        .namespace()
        .ok_or("missing DOCX namespace")?;
    if root.tag_name().name() != "document"
        || ![WORD_NS, "http://purl.oclc.org/ooxml/wordprocessingml/main"].contains(&namespace)
    {
        return Err("package is not a supported DOCX document".into());
    }
    let body = root
        .children()
        .find(|n| n.has_tag_name((namespace, "body")))
        .ok_or("DOCX body is missing")?;
    let mut paragraphs = Vec::new();
    let mut total = 0;
    for paragraph in body
        .descendants()
        .filter(|n| n.has_tag_name((namespace, "p")))
    {
        if paragraphs.len() == MAX_PARAGRAPHS {
            return Err("DOCX exceeds 2000 paragraphs".into());
        }
        let mut text = String::new();
        for node in paragraph.descendants().filter(|n| n.is_element()) {
            if node
                .ancestors()
                .skip(1)
                .find(|n| n.has_tag_name((namespace, "p")))
                != Some(paragraph)
            {
                continue;
            }
            if node.tag_name().namespace() == Some(namespace) {
                match node.tag_name().name() {
                    "t" => {
                        for child in node.children().filter(|n| n.is_text()) {
                            text.push_str(child.text().unwrap_or(""));
                        }
                    }
                    "tab" => text.push('\t'),
                    "br" | "cr" => text.push('\n'),
                    _ => {}
                }
            }
        }
        add_text(&mut total, &text)?;
        paragraphs.push(text);
    }
    Ok(
        json!({"path":args.path,"format":"docx","paragraphs":paragraphs,
        "warnings":["Text extraction only. Formatting, numbering, fields, revision semantics, tables, headers, footers, notes and drawings are not reconstructed."]}),
    )
}
