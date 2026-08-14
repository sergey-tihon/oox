//! Lightweight Word package summary.
use std::io::{self, Read};

use quick_xml::{events::Event, Reader};

use super::{
    append_decoded_reference, append_decoded_text, clean_summary_text, element_is,
    push_summary_line, read_part, relationship_count, validate_xml_bytes, DetailsView,
    MAX_SUMMARY_ITEMS, MAX_SUMMARY_TEXT_CHARS,
};
use crate::package::{xml_attribute, PackageIndex};

pub(super) fn build_word_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<DetailsView> {
    let document = read_part(archive, index, "/word/document.xml")?;
    validate_xml_bytes(&document, "/word/document.xml")?;
    let (paragraph_count, table_count, headings) = parse_word_document(&document);
    let image_count = relationship_count(index, "/word/document.xml", "/image");
    let mut summary = DetailsView {
        text: String::new(),
        links: Vec::new(),
    };
    push_summary_line(&mut summary, "Word summary", None);
    push_summary_line(&mut summary, "", None);
    push_summary_line(
        &mut summary,
        "Document: /word/document.xml",
        Some("/word/document.xml"),
    );
    push_summary_line(&mut summary, format!("Paragraphs: {paragraph_count}"), None);
    push_summary_line(&mut summary, format!("Tables: {table_count}"), None);
    push_summary_line(&mut summary, format!("Images: {image_count}"), None);
    push_summary_line(&mut summary, "", None);
    push_summary_line(&mut summary, "Heading outline", None);
    if headings.is_empty() {
        push_summary_line(&mut summary, "  (none)", None);
    } else {
        for (level, heading) in headings {
            push_summary_line(
                &mut summary,
                format!("  {}{}", "  ".repeat(level), heading),
                None,
            );
        }
    }
    Ok(summary)
}

fn parse_word_document(xml: &[u8]) -> (usize, usize, Vec<(usize, String)>) {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut paragraph_count = 0;
    let mut table_count = 0;
    let mut headings = Vec::new();
    let mut in_paragraph = false;
    let mut paragraph_text = String::new();
    let mut paragraph_style = None;
    let mut text_element = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"p") {
                    paragraph_count += 1;
                    in_paragraph = true;
                    paragraph_text.clear();
                    paragraph_style = None;
                } else if element_is(name.as_ref(), b"tbl") {
                    table_count += 1;
                } else if in_paragraph && element_is(name.as_ref(), b"pStyle") {
                    paragraph_style = xml_attribute(&event, b"w:val");
                } else if in_paragraph && element_is(name.as_ref(), b"t") {
                    text_element = true;
                }
            }
            Ok(Event::Empty(event)) => {
                let name = event.name();
                if in_paragraph && element_is(name.as_ref(), b"pStyle") {
                    paragraph_style = xml_attribute(&event, b"w:val");
                } else if element_is(name.as_ref(), b"tbl") {
                    table_count += 1;
                }
            }
            Ok(Event::Text(event)) if text_element => {
                append_decoded_text(&mut paragraph_text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::GeneralRef(event)) if text_element => {
                append_decoded_reference(&mut paragraph_text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::End(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"t") {
                    text_element = false;
                } else if element_is(name.as_ref(), b"p") {
                    if let Some(style) = paragraph_style.as_deref() {
                        if let Some(level) = style
                            .strip_prefix("Heading")
                            .and_then(|value| value.parse::<usize>().ok())
                        {
                            let heading = clean_summary_text(&paragraph_text);
                            if !heading.is_empty() && headings.len() < MAX_SUMMARY_ITEMS {
                                headings.push((level, heading));
                            }
                        }
                    }
                    in_paragraph = false;
                    paragraph_text.clear();
                    paragraph_style = None;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    (paragraph_count, table_count, headings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_summary_parser_extracts_paragraphs_tables_and_headings() {
        let word = br#"
            <w:document xmlns:w="word">
              <w:body>
                <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p>
                <w:p><w:r><w:t>Body text</w:t></w:r></w:p>
                <w:tbl/>
              </w:body>
            </w:document>
        "#;
        let (paragraphs, tables, headings) = parse_word_document(word);
        assert_eq!(paragraphs, 2);
        assert_eq!(tables, 1);
        assert_eq!(headings, vec![(1, "Overview".to_string())]);
    }
}
