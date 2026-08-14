//! Domain model and parsers for lightweight document summaries.
//!
//! The per-format parsers live in the `ppt`, `word`, and `excel` submodules;
//! this module holds the shared view model, limits, and XML helpers.
mod excel;
mod ppt;
mod word;

use std::io::{self, Read};

use quick_xml::{events::Event, Reader};

use crate::package::{PackageIndex, MAX_ENTRY_BYTES};

pub const MAX_SUMMARY_LINES: usize = 4096;
pub const MAX_SUMMARY_CHARS: usize = 512 * 1024;
pub const MAX_SUMMARY_ITEMS: usize = 4096;
pub const MAX_SUMMARY_TEXT_CHARS: usize = 16 * 1024;
pub const MAX_SHARED_STRINGS: usize = 16_384;

#[derive(Clone, Debug)]
pub struct DetailLink {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub target: String,
}

#[derive(Clone, Debug, Default)]
pub struct DetailsView {
    pub text: String,
    pub links: Vec<DetailLink>,
}

pub(crate) fn build_document_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<Option<DetailsView>> {
    if index.parts.contains_key("/ppt/presentation.xml") {
        return Ok(Some(ppt::build_ppt_summary(archive, index)?));
    }
    if index.parts.contains_key("/word/document.xml") {
        return Ok(Some(word::build_word_summary(archive, index)?));
    }
    if index.parts.contains_key("/xl/workbook.xml") {
        return Ok(Some(excel::build_excel_summary(archive, index)?));
    }
    Ok(None)
}

fn read_part<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
    path: &str,
) -> io::Result<Vec<u8>> {
    index.read_part(archive, path, MAX_ENTRY_BYTES)
}

fn validate_xml_bytes(bytes: &[u8], part: &str) -> io::Result<()> {
    let mut reader = Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => return Ok(()),
            Ok(_) => buffer.clear(),
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{part}: malformed XML: {error}"),
                ));
            }
        }
    }
}

fn element_is(name: &[u8], expected: &[u8]) -> bool {
    name.rsplit(|byte| *byte == b':').next() == Some(expected)
}

fn relationship_target_for_id(
    index: &PackageIndex,
    source: &str,
    relationship_id: &str,
) -> Option<String> {
    index.outgoing.get(source).and_then(|relationships| {
        relationships.iter().find_map(|relationship_index| {
            let relationship = &index.relationships[*relationship_index];
            (relationship.id == relationship_id)
                .then(|| relationship.resolved_target.clone())
                .flatten()
        })
    })
}

fn relationship_count(index: &PackageIndex, source: &str, suffix: &str) -> usize {
    index
        .outgoing
        .get(source)
        .map(|relationships| {
            relationships
                .iter()
                .filter(|relationship_index| {
                    index.relationships[**relationship_index]
                        .relationship_type
                        .ends_with(suffix)
                })
                .count()
        })
        .unwrap_or(0)
}

fn append_decoded_reference(
    output: &mut String,
    event: &quick_xml::events::BytesRef<'_>,
    limit: usize,
) {
    let Ok(reference) = event.decode() else {
        return;
    };
    let encoded = format!("&{};", reference);
    let Ok(decoded) = quick_xml::escape::unescape(&encoded) else {
        return;
    };
    let remaining = limit.saturating_sub(output.chars().count());
    output.extend(decoded.chars().take(remaining));
}

fn append_decoded_text(
    output: &mut String,
    event: &quick_xml::events::BytesText<'_>,
    limit: usize,
) {
    if output.chars().count() >= limit {
        return;
    }
    let decoded = event
        .decode()
        .ok()
        .and_then(|value| {
            quick_xml::escape::unescape(value.as_ref())
                .ok()
                .map(|unescaped| unescaped.into_owned())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(event.as_ref()).into_owned());
    let remaining = limit.saturating_sub(output.chars().count());
    output.extend(decoded.chars().take(remaining));
}

fn clean_summary_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(MAX_SUMMARY_TEXT_CHARS).collect()
}

fn push_summary_line(view: &mut DetailsView, line: impl AsRef<str>, target: Option<&str>) {
    if view.text.lines().count() >= MAX_SUMMARY_LINES || view.text.len() >= MAX_SUMMARY_CHARS {
        return;
    }
    let line = line.as_ref();
    let remaining = MAX_SUMMARY_CHARS.saturating_sub(view.text.len() + 1);
    let mut bounded = String::new();
    for character in line.chars() {
        if bounded.len() + character.len_utf8() > remaining {
            break;
        }
        bounded.push(character);
    }
    if bounded.chars().count() < line.chars().count() {
        bounded.push('…');
    }
    let line_number = view.text.lines().count();
    view.text.push_str(&bounded);
    view.text.push('\n');
    let Some(target) = target else {
        return;
    };
    let Some(byte_start) = line.find(target) else {
        return;
    };
    let start = line[..byte_start].chars().count();
    view.links.push(DetailLink {
        line: line_number,
        start,
        end: start + target.chars().count(),
        target: target.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_xml_is_rejected_with_part_context() {
        let error = validate_xml_bytes(b"<root><item></root>", "/ppt/presentation.xml")
            .expect_err("summary validation must reject malformed XML");
        assert!(error.to_string().contains("/ppt/presentation.xml"));
        assert!(validate_xml_bytes(b"<sst><si><t>bad</sst>", "/xl/sharedStrings.xml").is_err());
    }

    #[test]
    fn summary_text_decodes_entities_and_stays_bounded() {
        let text = ppt::extract_all_text(br#"<root>one &amp; &lt;two&gt;</root>"#);
        assert_eq!(text, "one & <two>");

        let mut view = DetailsView {
            text: String::new(),
            links: Vec::new(),
        };
        for _ in 0..(MAX_SUMMARY_LINES * 2) {
            push_summary_line(&mut view, "summary line", None);
        }
        assert!(view.text.len() <= MAX_SUMMARY_CHARS);
        assert!(view.text.lines().count() <= MAX_SUMMARY_LINES);
    }
}
