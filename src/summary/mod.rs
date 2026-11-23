//! Domain model and parsers for lightweight document summaries.
//!
//! The per-format parsers live in the `ppt`, `word`, and `excel` submodules;
//! this module holds the shared view model, limits, and XML helpers.
mod excel;
mod ppt;
mod word;

use std::io::{self, Read};

use crate::cancel::Cancel;
use quick_xml::{Reader, events::Event};

use crate::package::{MAX_ENTRY_BYTES, OFFICE_DOCUMENT_REL_TYPES, PackageIndex};

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

#[derive(Clone, Copy)]
enum DocumentKind {
    Presentation,
    Word,
    Excel,
}

fn document_kind(content_type: &str) -> Option<DocumentKind> {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !content_type.ends_with(".main+xml") {
        return None;
    }
    if content_type.contains("presentationml")
        || content_type.starts_with("application/vnd.ms-powerpoint.")
    {
        Some(DocumentKind::Presentation)
    } else if content_type.contains("wordprocessingml")
        || content_type.starts_with("application/vnd.ms-word.")
    {
        Some(DocumentKind::Word)
    } else if content_type.contains("spreadsheetml")
        || content_type.starts_with("application/vnd.ms-excel.")
    {
        Some(DocumentKind::Excel)
    } else {
        None
    }
}

pub(crate) fn build_document_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
    cancel: &Cancel,
) -> io::Result<Option<DetailsView>> {
    let Some(main) = index.related_part("/", &OFFICE_DOCUMENT_REL_TYPES) else {
        return Ok(None);
    };
    let Some(kind) = main.content_type.as_deref().and_then(document_kind) else {
        return Ok(None);
    };
    let main_path = &main.path;
    let summary = match kind {
        DocumentKind::Presentation => ppt::build_ppt_summary(archive, index, main_path, cancel)?,
        DocumentKind::Word => word::build_word_summary(archive, index, main_path)?,
        DocumentKind::Excel => excel::build_excel_summary(archive, index, main_path, cancel)?,
    };
    Ok(Some(summary))
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

fn element_is(name: &str, expected: &[u8]) -> bool {
    name.rsplit(':')
        .next()
        .is_some_and(|local_name| local_name.as_bytes() == expected)
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

pub(super) struct SummaryText {
    text: String,
    chars: usize,
}

impl SummaryText {
    pub(super) fn new() -> Self {
        Self {
            text: String::new(),
            chars: 0,
        }
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.chars = 0;
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }

    pub(super) fn into_string(self) -> String {
        self.text
    }

    pub(super) fn push_text(&mut self, event: &quick_xml::events::BytesText<'_>) {
        if self.chars >= MAX_SUMMARY_TEXT_CHARS {
            return;
        }
        let decoded =
            quick_xml::escape::unescape(event.as_ref()).unwrap_or_else(|_| event.as_ref().into());
        for character in decoded.chars().take(MAX_SUMMARY_TEXT_CHARS - self.chars) {
            self.text.push(character);
            self.chars += 1;
        }
    }

    pub(super) fn push_reference(&mut self, event: &quick_xml::events::BytesRef<'_>) {
        if self.chars >= MAX_SUMMARY_TEXT_CHARS {
            return;
        }
        let encoded = format!("&{};", event.as_ref());
        let Ok(decoded) = quick_xml::escape::unescape(&encoded) else {
            return;
        };
        for character in decoded.chars().take(MAX_SUMMARY_TEXT_CHARS - self.chars) {
            self.text.push(character);
            self.chars += 1;
        }
    }
}

fn clean_summary_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(MAX_SUMMARY_TEXT_CHARS).collect()
}

fn normalized_line_char_count(text: &str) -> usize {
    let mut characters = text.chars().peekable();
    let mut count = 0;
    while let Some(character) = characters.next() {
        if character == '\r' && characters.peek() == Some(&'\n') {
            characters.next();
        }
        count += 1;
    }
    count
}

pub(super) struct SummaryBuilder {
    view: DetailsView,
    lines: usize,
}

impl SummaryBuilder {
    pub(super) fn new() -> Self {
        Self {
            view: DetailsView {
                text: String::new(),
                links: Vec::new(),
            },
            lines: 0,
        }
    }

    pub(super) fn push_line(&mut self, line: impl AsRef<str>, target: Option<&str>) {
        if self.lines >= MAX_SUMMARY_LINES || self.view.text.len() >= MAX_SUMMARY_CHARS {
            return;
        }
        let line = line.as_ref();
        let remaining = MAX_SUMMARY_CHARS.saturating_sub(self.view.text.len() + 1);
        let mut bounded = String::new();
        let mut characters = line.chars().peekable();
        let mut truncated = false;
        while let Some(mut character) = characters.next() {
            if character == '\r' {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                character = ' ';
            } else if character == '\n' {
                character = ' ';
            }
            if bounded.len() + character.len_utf8() > remaining {
                truncated = true;
                break;
            }
            bounded.push(character);
        }
        if truncated {
            bounded.push('…');
        }
        let line_number = self.lines;
        self.view.text.push_str(&bounded);
        self.view.text.push('\n');
        self.lines += 1;
        let Some(target) = target else {
            return;
        };
        let Some(byte_start) = line.find(target) else {
            return;
        };
        let start = normalized_line_char_count(&line[..byte_start]);
        self.view.links.push(DetailLink {
            line: line_number,
            start,
            end: start + normalized_line_char_count(target),
            target: target.to_string(),
        });
    }

    pub(super) fn finish(self) -> DetailsView {
        self.view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn indexed_package(
        entries: &[(&str, &str)],
    ) -> (PackageIndex, zip::ZipArchive<std::io::Cursor<Vec<u8>>>) {
        use std::io::{Cursor, Write as _};

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let mut index_archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        let index = PackageIndex::from_archive(&mut index_archive).unwrap();
        let archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (index, archive)
    }

    #[test]
    fn malformed_xml_is_rejected_with_part_context() {
        let error = validate_xml_bytes(b"<root><item></root>", "/ppt/presentation.xml")
            .expect_err("summary validation must reject malformed XML");
        assert!(error.to_string().contains("/ppt/presentation.xml"));
        assert!(validate_xml_bytes(b"<sst><si><t>bad</sst>", "/xl/sharedStrings.xml").is_err());
    }

    #[test]
    fn summary_text_decodes_entities_and_stays_bounded() {
        let text = ppt::extract_all_text(br#"<root>one &amp; &lt;two&gt;&#x21;</root>"#);
        assert_eq!(text, "one & <two>!");

        let mut builder = SummaryBuilder::new();
        for _ in 0..(MAX_SUMMARY_LINES * 2) {
            builder.push_line("summary line", None);
        }
        let view = builder.finish();
        assert!(view.text.len() <= MAX_SUMMARY_CHARS);
        assert!(view.text.lines().count() <= MAX_SUMMARY_LINES);
    }

    #[test]
    fn summary_text_stops_at_the_char_limit() {
        let event = quick_xml::events::BytesText::from_escaped("é".repeat(10_000));
        let mut text = SummaryText::new();
        text.push_text(&event);
        text.push_text(&event);

        assert_eq!(text.chars, MAX_SUMMARY_TEXT_CHARS);
        assert_eq!(text.as_str().chars().count(), MAX_SUMMARY_TEXT_CHARS);
    }

    #[test]
    fn summary_builder_normalizes_embedded_newlines() {
        let mut builder = SummaryBuilder::new();
        builder.push_line("A1: first\r\nB1: second\nthird", Some("B1"));
        builder.push_line("C1: value\nmore", Some("C1"));
        let view = builder.finish();

        assert_eq!(view.text, "A1: first B1: second third\nC1: value more\n");
        assert_eq!(view.text.lines().count(), 2);
        assert_eq!(
            view.links.iter().map(|link| link.line).collect::<Vec<_>>(),
            [0, 1]
        );
        let first_line = view.text.lines().next().unwrap();
        assert_eq!(
            first_line
                .chars()
                .skip(view.links[0].start)
                .take(2)
                .collect::<String>(),
            "B1"
        );

        let mut builder = SummaryBuilder::new();
        builder.push_line("\n".repeat(MAX_SUMMARY_LINES + 1), None);
        assert_eq!(builder.finish().text.lines().count(), 1);
    }

    #[test]
    fn summary_finds_main_part_through_root_relationship() -> io::Result<()> {
        let (index, mut archive) = indexed_package(&[
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/custom/main.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="custom/main.xml"/>
</Relationships>"#,
            ),
            (
                "custom/main.xml",
                r#"<w:document xmlns:w="w"><w:body/></w:document>"#,
            ),
        ]);

        let summary = build_document_summary(&mut archive, &index, &Cancel::never())?.unwrap();
        assert!(summary.text.contains("Word summary"));
        assert!(summary.text.contains("Document: /custom/main.xml"));
        Ok(())
    }

    #[test]
    fn excel_summary_uses_strict_shared_strings_relationship() -> io::Result<()> {
        let (index, mut archive) = indexed_package(&[
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/custom/book.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId0" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="custom/book.xml"/>
</Relationships>"#,
            ),
            (
                "custom/book.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="S" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "custom/_rels/book.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="sheet.xml"/>
  <Relationship Id="rId2" Type="http://purl.oclc.org/ooxml/officeDocument/relationships/sharedStrings" Target="strings.xml"/>
</Relationships>"#,
            ),
            ("custom/strings.xml", "<sst><si><t>Hello</t></si></sst>"),
            (
                "custom/sheet.xml",
                "<worksheet><sheetData><row><c r=\"A1\" t=\"s\"><v>0</v></c></row></sheetData></worksheet>",
            ),
        ]);

        let summary = build_document_summary(&mut archive, &index, &Cancel::never())?.unwrap();
        assert!(summary.text.contains("A1 = Hello"), "{}", summary.text);
        Ok(())
    }

    #[test]
    fn package_without_office_document_has_no_summary() -> io::Result<()> {
        let (index, mut archive) = indexed_package(&[("custom/main.xml", "<document/>")]);
        assert!(build_document_summary(&mut archive, &index, &Cancel::never())?.is_none());
        Ok(())
    }
}
