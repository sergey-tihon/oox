//! Lightweight PowerPoint package summary.
use std::io::{self, Read};

use quick_xml::{events::Event, Reader};

use super::{
    append_decoded_reference, append_decoded_text, clean_summary_text, element_is,
    push_summary_line, read_part, relationship_count, relationship_target_for_id,
    validate_xml_bytes, DetailsView, MAX_SUMMARY_ITEMS, MAX_SUMMARY_TEXT_CHARS,
};
use crate::package::{xml_attribute, PackageIndex};

pub(super) fn build_ppt_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<DetailsView> {
    let presentation = read_part(archive, index, "/ppt/presentation.xml")?;
    validate_xml_bytes(&presentation, "/ppt/presentation.xml")?;
    let slide_paths = parse_ppt_slide_order(&presentation, index);
    let mut summary = DetailsView {
        text: String::new(),
        links: Vec::new(),
    };
    push_summary_line(&mut summary, "PowerPoint summary", None);
    push_summary_line(&mut summary, "", None);
    push_summary_line(&mut summary, format!("Slides: {}", slide_paths.len()), None);

    for (number, slide_path) in slide_paths.iter().enumerate() {
        let slide = read_part(archive, index, slide_path)?;
        validate_xml_bytes(&slide, slide_path)?;
        let title = extract_ppt_title(&slide).unwrap_or_else(|| "(untitled)".to_string());
        let image_count = relationship_count(index, slide_path, "/image");
        let notes_path = index.outgoing.get(slide_path).and_then(|relationships| {
            relationships.iter().find_map(|relationship_index| {
                let relationship = &index.relationships[*relationship_index];
                if relationship.relationship_type.ends_with("/notesSlide") {
                    relationship.resolved_target.clone()
                } else {
                    None
                }
            })
        });
        let notes_text = match notes_path.as_deref() {
            Some(path) => {
                let bytes = read_part(archive, index, path)?;
                validate_xml_bytes(&bytes, path)?;
                let text = clean_summary_text(&extract_all_text(&bytes));
                (!text.is_empty()).then_some(text)
            }
            None => None,
        };

        push_summary_line(&mut summary, "", None);
        push_summary_line(&mut summary, format!("{}. {}", number + 1, title), None);
        push_summary_line(
            &mut summary,
            format!("   Part: {slide_path}"),
            Some(slide_path),
        );
        push_summary_line(&mut summary, format!("   Images: {image_count}"), None);
        match notes_path {
            Some(path) => {
                let suffix = notes_text
                    .as_deref()
                    .map_or_else(String::new, |text| format!(" — {text}"));
                push_summary_line(
                    &mut summary,
                    format!("   Notes: {path}{suffix}"),
                    Some(&path),
                );
            }
            None => push_summary_line(&mut summary, "   Notes: none", None),
        }
    }

    Ok(summary)
}

fn parse_ppt_slide_order(xml: &[u8], index: &PackageIndex) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut slides = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if element_is(event.name().as_ref(), b"sldId") =>
            {
                if let Some(relationship_id) = xml_attribute(&event, b"r:id") {
                    if let Some(path) =
                        relationship_target_for_id(index, "/ppt/presentation.xml", &relationship_id)
                    {
                        if slides.len() < MAX_SUMMARY_ITEMS {
                            slides.push(path);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    slides
}

fn extract_ppt_title(xml: &[u8]) -> Option<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut shape_depth = 0usize;
    let mut title_shape = false;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                if element_is(event.name().as_ref(), b"sp") {
                    shape_depth += 1;
                    if shape_depth == 1 {
                        title_shape = false;
                        text.clear();
                    }
                } else if element_is(event.name().as_ref(), b"ph")
                    && shape_depth > 0
                    && xml_attribute(&event, b"type").is_some_and(|value| {
                        value.eq_ignore_ascii_case("title")
                            || value.eq_ignore_ascii_case("ctrTitle")
                    })
                {
                    title_shape = true;
                }
            }
            Ok(Event::Empty(event))
                if element_is(event.name().as_ref(), b"ph")
                    && shape_depth > 0
                    && xml_attribute(&event, b"type").is_some_and(|value| {
                        value.eq_ignore_ascii_case("title")
                            || value.eq_ignore_ascii_case("ctrTitle")
                    }) =>
            {
                title_shape = true;
            }
            Ok(Event::Text(event)) if title_shape => {
                append_decoded_text(&mut text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::GeneralRef(event)) if title_shape => {
                append_decoded_reference(&mut text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::End(event)) if element_is(event.name().as_ref(), b"sp") => {
                if shape_depth == 1 && title_shape {
                    let title = clean_summary_text(&text);
                    if !title.is_empty() {
                        return Some(title);
                    }
                }
                shape_depth = shape_depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    None
}

pub(super) fn extract_all_text(xml: &[u8]) -> String {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Text(event)) => {
                append_decoded_text(&mut text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::GeneralRef(event)) => {
                append_decoded_reference(&mut text, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    text
}
