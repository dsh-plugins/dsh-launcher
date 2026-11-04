use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};
use roxmltree::{Document, ParsingOptions};
use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub(crate) const MAX_XML_BYTES: usize = 2 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
pub(crate) const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
pub(crate) const REL_NS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub(crate) const STRICT_REL_NS: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";
pub(crate) const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
pub(crate) const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

pub(crate) struct Package {
    archive: ZipArchive<Cursor<Vec<u8>>>,
}
pub(crate) struct Relationship {
    pub id: String,
    pub kind: String,
    pub target: String,
}

impl Package {
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        let mut archive =
            ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("invalid Office ZIP: {e}"))?;
        if archive.len() > MAX_ENTRIES {
            return Err("package exceeds 256 ZIP entries".into());
        }
        let mut expanded = 0u64;
        let mut names = HashSet::new();
        let mut relationships = Vec::new();
        for index in 0..archive.len() {
            let file = archive
                .by_index(index)
                .map_err(|e| format!("unsupported ZIP entry: {e}"))?;
            let name = file.name();
            let clean_name = name.strip_suffix('/').unwrap_or(name);
            if clean_name.is_empty()
                || !safe_part_path(clean_name)
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err("unsafe or duplicate ZIP entry name".into());
            }
            if file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err("ZIP symlinks are unsupported".into());
            }
            expanded = expanded
                .checked_add(file.size())
                .ok_or("ZIP size overflow")?;
            if expanded > MAX_EXPANDED_BYTES {
                return Err("package expands beyond 32 MiB".into());
            }
            if (name.ends_with(".xml") || name.ends_with(".rels"))
                && file.size() > MAX_XML_BYTES as u64
            {
                return Err("XML part exceeds 2 MiB".into());
            }
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".bin")
                || lower.contains("/embeddings/")
                || lower.contains("vbaproject")
                || lower.contains("/externallinks/")
            {
                return Err("macros, embedded objects and external links are unsupported".into());
            }
            if name.ends_with(".rels") {
                relationships.push(name.to_owned());
            }
        }
        let mut package = Self { archive };
        let types = package.xml("[Content_Types].xml")?;
        let document = parse_xml(&types)?;
        if !document.root_element().has_tag_name((
            "http://schemas.openxmlformats.org/package/2006/content-types",
            "Types",
        )) {
            return Err("invalid Office content types".into());
        }
        if document
            .descendants()
            .filter_map(|n| n.attribute("ContentType"))
            .any(|s| {
                let s = s.to_ascii_lowercase();
                s.contains("macroenabled") || s.contains("vbaproject") || s.contains("oleobject")
            })
        {
            return Err("active Office content is unsupported".into());
        }
        // Validate every relationship, including ones not used for extraction.
        for name in relationships {
            package.relationships(&name)?;
        }
        Ok(package)
    }

    pub fn xml(&mut self, name: &str) -> Result<String, String> {
        let file = self
            .archive
            .by_name(name)
            .map_err(|_| format!("missing or unreadable part: {name}"))?;
        if file.size() > MAX_XML_BYTES as u64 {
            return Err("XML part exceeds 2 MiB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_XML_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read XML part: {e}"))?;
        if bytes.len() > MAX_XML_BYTES {
            return Err("XML part exceeds 2 MiB".into());
        }
        String::from_utf8(bytes).map_err(|_| "only UTF-8 XML parts are supported".into())
    }

    pub fn relationships(&mut self, name: &str) -> Result<Vec<Relationship>, String> {
        let xml = self.xml(name)?;
        let document = parse_xml(&xml)?;
        if !document
            .root_element()
            .has_tag_name((PKG_REL_NS, "Relationships"))
        {
            return Err("invalid relationship document".into());
        }
        let base = if name.starts_with("_rels/") {
            ""
        } else {
            name.rsplit_once("/_rels/")
                .map(|(base, _)| base)
                .ok_or("invalid relationship part path")?
        };
        let mut ids = HashSet::new();
        let mut result = Vec::new();
        for node in document
            .root_element()
            .children()
            .filter(|n| n.is_element())
        {
            if !node.has_tag_name((PKG_REL_NS, "Relationship")) {
                return Err("invalid relationship element".into());
            }
            let id = node.attribute("Id").ok_or("relationship Id is missing")?;
            let kind = node
                .attribute("Type")
                .ok_or("relationship Type is missing")?;
            let target = node
                .attribute("Target")
                .ok_or("relationship Target is missing")?;
            if id.is_empty() || !ids.insert(id.to_owned()) {
                return Err("duplicate or empty relationship Id".into());
            }
            if node
                .attribute("TargetMode")
                .is_some_and(|s| s != "Internal")
            {
                return Err("external relationships are unsupported".into());
            }
            if [
                "vbaProject",
                "oleObject",
                "externalLink",
                "attachedTemplate",
            ]
            .iter()
            .any(|part| kind.ends_with(part))
            {
                return Err("active or external Office relationships are unsupported".into());
            }
            result.push(Relationship {
                id: id.to_owned(),
                kind: kind.to_owned(),
                target: resolve_target(base, target)?,
            });
        }
        Ok(result)
    }

    pub fn main_part(&mut self) -> Result<String, String> {
        let rels = self.relationships("_rels/.rels")?;
        let mut mains = rels
            .iter()
            .filter(|r| is_relationship(&r.kind, "officeDocument"));
        let result = mains
            .next()
            .ok_or("missing officeDocument relationship")?
            .target
            .clone();
        if mains.next().is_some() {
            return Err("multiple officeDocument relationships".into());
        }
        Ok(result)
    }
}

pub(crate) fn parse_xml(xml: &str) -> Result<Document<'_>, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("XML part exceeds 2 MiB".into());
    }
    preflight_xml(xml)?;
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: 100_000,
    };
    Document::parse_with_options(xml, options)
        .map_err(|e| format!("invalid or unsupported XML: {e}"))
}

pub(crate) fn is_relationship(kind: &str, name: &str) -> bool {
    kind.strip_prefix(REL_NS)
        .or_else(|| kind.strip_prefix(STRICT_REL_NS))
        .and_then(|s| s.strip_prefix('/'))
        == Some(name)
}

pub(crate) fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn safe_part_path(path: &str) -> bool {
    path.len() <= 1024
        && !path.contains(['\\', ':', '\0', '%', '?', '#'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

fn resolve_target(base: &str, target: &str) -> Result<String, String> {
    if target.is_empty()
        || target.starts_with("//")
        || target.len() > 1024
        || target.contains(['\\', ':', '\0', '%', '?', '#'])
        || target.chars().any(char::is_control)
    {
        return Err("unsupported relationship target".into());
    }
    let mut parts: Vec<&str> = if target.starts_with('/') || base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => return Err("ambiguous relationship target".into()),
            ".." => {
                parts.pop().ok_or("relationship escapes the package")?;
            }
            _ => parts.push(part),
        }
    }
    let result = parts.join("/");
    if !safe_part_path(&result) {
        return Err("unsafe relationship target".into());
    }
    Ok(result)
}

pub(crate) fn zip_parts(parts: Vec<(String, String)>) -> Result<Vec<u8>, String> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, xml) in parts {
        parse_xml(&xml)?;
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        zip.write_all(xml.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(zip.finish().map_err(|e| e.to_string())?.into_inner())
}

pub(crate) fn escape(text: &str) -> Result<String, String> {
    if text.chars().any(|c| !matches!(c, '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err("text contains a character forbidden by XML 1.0".into());
    }
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        .replace('\r', "&#13;"))
}

pub(crate) fn root_relationship(target: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Id="rId1" Type="{REL_NS}/officeDocument" Target="{target}"/></Relationships>"#
    )
}

// roxmltree performs pairwise duplicate-attribute and namespace checks. Bound
// those inputs with a streaming tokenizer first, disabling its own duplicate
// attribute checks. This is XML tokenization, not regex or substring parsing.
fn preflight_xml(xml: &str) -> Result<(), String> {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0usize;
    let mut namespaces = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("invalid XML token: {e}"))?
        {
            Event::Start(element) => {
                depth += 1;
                if depth > 128 {
                    return Err("XML nesting exceeds 128 levels".into());
                }
                check_attributes(&element, &mut namespaces)?;
            }
            Event::Empty(element) => {
                if depth == 128 {
                    return Err("XML nesting exceeds 128 levels".into());
                }
                check_attributes(&element, &mut namespaces)?;
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or("unbalanced XML end tag")?;
            }
            Event::DocType(_) => return Err("XML DTDs are unsupported".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}
fn check_attributes(element: &BytesStart<'_>, namespaces: &mut usize) -> Result<(), String> {
    for (index, attribute) in element.attributes().with_checks(false).enumerate() {
        if index >= 64 {
            return Err("XML element exceeds 64 attributes".into());
        }
        let attribute = attribute.map_err(|e| format!("invalid XML attribute: {e}"))?;
        let name = attribute.key.as_ref();
        if name == b"xmlns" || name.starts_with(b"xmlns:") {
            *namespaces += 1;
            if *namespaces > 128 {
                return Err("XML part exceeds 128 namespace declarations".into());
            }
        }
    }
    Ok(())
}
