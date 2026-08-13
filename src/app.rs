use std::{
    fmt::Write as _,
    io::{self, Cursor, Read},
    path::PathBuf,
};

use edtui::{EditorState, Lines};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use quick_xml::{events::Event, Reader, Writer};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use tui_tree_widget::{TreeItem, TreeState};

use crate::package::{
    xml_attribute, Package, PackageIndex, PartKind, Relationship, TargetMode, MAX_PATH_DEPTH,
};
use crate::summary::{DetailLink, DetailsView};
use crate::worker::{accepts_result, Job, ResultMessage, Worker};

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub path: String,
    pub children: Vec<Node>,
}

impl Node {
    fn new(name: &str, path: &str) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            children: Vec::<Self>::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Empty,
    Xml,
    PlainText,
    Json,
    Hex,
    Image,
    Summary,
    Info,
    Error,
}

#[derive(Debug)]
pub(crate) enum Preview {
    Editor { kind: PreviewKind, text: String },
    Image(DynamicImage),
    Info(String),
    Error(String),
}

const MAX_HEX_PREVIEW_BYTES: usize = 1024 * 1024;
const MAX_XML_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_SUMMARY_LINES: usize = 4096;
const MAX_SUMMARY_CHARS: usize = 512 * 1024;
const MAX_SUMMARY_ITEMS: usize = 4096;
const MAX_SUMMARY_TEXT_CHARS: usize = 16 * 1024;
const MAX_SHARED_STRINGS: usize = 16_384;

pub struct App {
    pub file_path: String,
    pub tree_state: TreeState<String>,
    pub tree_items: Vec<TreeItem<'static, String>>,
    pub editor_state: EditorState,
    pub image_state: Option<StatefulProtocol>,
    pub preview_kind: PreviewKind,
    pub picker: Picker,
    pub current_widget: CurrentWidget,
    pub status_message: Option<String>,
    pub details_visible: bool,
    pub details_scroll: u16,
    pub details_cursor: usize,
    pub package_index: PackageIndex,
    pub document_summary: Option<DetailsView>,
    package: Option<Package>,
    worker: Worker,
    open_request_id: u64,
    preview_request_id: u64,
    pub loading: bool,
    pub worker_error: Option<String>,
    // The legacy synchronous constructor exists only for in-tree unit tests.
    #[cfg(test)]
    synchronous: bool,
    pub summary_visible: bool,
    pub summary_scroll: u16,
    navigation_back: Vec<String>,
    navigation_forward: Vec<String>,
    navigation_current: Option<String>,
    pub show_help: bool,
    pub search_active: bool,
    pub search_query: String,
    search_matches: Vec<String>,
    search_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurrentWidget {
    Tree,
    Details,
    TextArea,
}

fn part_kind_label(kind: &PartKind) -> &'static str {
    match kind {
        PartKind::Xml => "XML",
        PartKind::Image => "Image",
        PartKind::Binary => "Binary/unsupported",
        PartKind::Directory => "Directory",
    }
}

fn relationship_target_label(relationship: &Relationship) -> String {
    match relationship.target_mode {
        TargetMode::External => format!("{} (external)", relationship.target),
        TargetMode::Internal => relationship
            .resolved_target
            .clone()
            .unwrap_or_else(|| relationship.target.clone()),
    }
}

fn relationship_type_label(relationship: &Relationship) -> String {
    relationship
        .relationship_type
        .rsplit('/')
        .next()
        .unwrap_or(&relationship.relationship_type)
        .to_string()
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let compact = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

fn push_detail_line(text: &mut String, line: &str) -> usize {
    let line_number = text.lines().count();
    text.push_str(line);
    text.push('\n');
    line_number
}

fn pretty_print_json(value: &serde_json::Value) -> io::Result<String> {
    struct LimitedWriter {
        output: Vec<u8>,
        limit: usize,
    }

    impl io::Write for LimitedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.output.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("formatted JSON preview exceeds {MAX_JSON_PREVIEW_BYTES} byte limit"),
                ));
            }
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut output = LimitedWriter {
        output: Vec::new(),
        limit: MAX_JSON_PREVIEW_BYTES,
    };
    serde_json::to_writer_pretty(&mut output, value).map_err(io::Error::other)?;
    String::from_utf8(output.output).map_err(io::Error::other)
}

pub(crate) fn build_preview(
    path: &str,
    content_type: Option<&str>,
    size: u64,
    compressed_size: u64,
    bytes: &[u8],
) -> Preview {
    if App::is_image(path) {
        let mut reader = ImageReader::with_format(Cursor::new(bytes), App::image_format(path));
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_IMAGE_PIXELS * 4);
        reader.limits(limits);
        return match reader.decode() {
            Ok(image)
                if u64::from(image.width()) * u64::from(image.height()) <= MAX_IMAGE_PIXELS =>
            {
                Preview::Image(image)
            }
            Ok(_) => Preview::Error(format!("image exceeds {MAX_IMAGE_PIXELS} pixel limit")),
            Err(error) => Preview::Error(format!("image decode failed: {error}")),
        };
    }

    if App::is_xml(path) {
        let text = String::from_utf8_lossy(bytes);
        return match App::pretty_print_xml(&text) {
            Ok(formatted) => Preview::Editor {
                kind: PreviewKind::Xml,
                text: formatted,
            },
            Err(error) => Preview::Error(format!("XML preview failed: {error}")),
        };
    }

    if is_json_file(path, content_type) {
        let Some(text) = utf8_text(bytes) else {
            return Preview::Error("JSON is not valid UTF-8".to_string());
        };
        let formatted = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => match pretty_print_json(&value) {
                Ok(formatted) => formatted,
                Err(error) => return Preview::Error(format!("JSON preview failed: {error}")),
            },
            Err(_) => text,
        };
        return Preview::Editor {
            kind: PreviewKind::Json,
            text: formatted,
        };
    }

    if is_ole_file(path, content_type) {
        return Preview::Info(binary_info(
            path,
            "OLE/VBA object",
            content_type,
            size,
            compressed_size,
        ));
    }

    if is_font_file(path, content_type) {
        return Preview::Info(binary_info(
            path,
            "Font",
            content_type,
            size,
            compressed_size,
        ));
    }

    if is_media_file(path, content_type) {
        return Preview::Info(binary_info(
            path,
            "Media",
            content_type,
            size,
            compressed_size,
        ));
    }

    if is_bin_file(path) {
        return Preview::Editor {
            kind: PreviewKind::Hex,
            text: format_hex_preview(bytes),
        };
    }

    if let Some(text) = utf8_text(bytes).filter(|text| is_probably_text(text)) {
        return Preview::Editor {
            kind: PreviewKind::PlainText,
            text,
        };
    }

    Preview::Info(binary_info(
        path,
        "Binary data",
        content_type,
        size,
        compressed_size,
    ))
}

fn is_json_file(path: &str, content_type: Option<&str>) -> bool {
    path_extension(path).is_some_and(|extension| extension == "json")
        || content_type.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value == "application/json" || value.ends_with("+json")
        })
}

fn is_bin_file(path: &str) -> bool {
    path_extension(path).is_some_and(|extension| extension == "bin")
}

fn is_font_file(path: &str, content_type: Option<&str>) -> bool {
    let extension_match = path_extension(path).is_some_and(|extension| {
        matches!(extension.as_str(), "ttf" | "otf" | "woff" | "woff2" | "eot")
    });
    extension_match || content_type.is_some_and(|value| value.to_ascii_lowercase().contains("font"))
}

fn is_media_file(path: &str, content_type: Option<&str>) -> bool {
    let extension_match = path_extension(path).is_some_and(|extension| {
        matches!(
            extension.as_str(),
            "mp3" | "wav" | "m4a" | "aac" | "ogg" | "mp4" | "avi" | "mov" | "wmv" | "mkv"
        )
    });
    let content_type_match = content_type.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("audio/") || value.starts_with("video/")
    });
    extension_match || content_type_match
}

fn is_ole_file(path: &str, content_type: Option<&str>) -> bool {
    let content_type_match = content_type.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("ole") || value.contains("vba") || value.contains("ms-office")
    });
    content_type_match
        || path.to_ascii_lowercase().contains("oleobject")
        || path.to_ascii_lowercase().contains("vbaproject")
}

fn path_extension(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn utf8_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(text).to_string())
}

fn is_probably_text(text: &str) -> bool {
    text.chars()
        .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
}

fn format_hex_preview(bytes: &[u8]) -> String {
    let shown = &bytes[..bytes.len().min(MAX_HEX_PREVIEW_BYTES)];
    let mut output = format_hex_dump(shown);
    if shown.len() < bytes.len() {
        let _ = writeln!(
            output,
            "\n[preview truncated: showing {} of {} bytes]",
            shown.len(),
            bytes.len()
        );
    }
    output
}

fn format_hex_dump(bytes: &[u8]) -> String {
    let mut output = String::new();
    for (line, chunk) in bytes.chunks(16).enumerate() {
        let offset = line * 16;
        let _ = write!(output, "{offset:08x}  ");
        for index in 0..16 {
            if index == 8 {
                output.push(' ');
            }
            if let Some(byte) = chunk.get(index) {
                let _ = write!(output, "{byte:02x} ");
            } else {
                output.push_str("   ");
            }
        }
        output.push_str(" |");
        for byte in chunk {
            output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }
        output.push('|');
        output.push('\n');
    }
    output
}

fn binary_info(
    path: &str,
    category: &str,
    content_type: Option<&str>,
    size: u64,
    compressed_size: u64,
) -> String {
    let mime_type = content_type.unwrap_or_else(|| fallback_mime_type(path));
    format!(
        "Binary file\n\nName:       {path}\nCategory:   {category}\nMIME type:  {mime_type}\nSize:       {size} bytes\nCompressed: {compressed_size} bytes\n\nRaw bytes are not rendered for this file type."
    )
}

fn fallback_mime_type(path: &str) -> &'static str {
    match path_extension(path).as_deref() {
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("mp4") => "video/mp4",
        Some("avi") => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

fn read_part<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
    path: &str,
) -> io::Result<Vec<u8>> {
    index.read_part(archive, path, crate::package::MAX_ENTRY_BYTES)
}

pub(crate) fn build_document_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<Option<DetailsView>> {
    if index.parts.contains_key("/ppt/presentation.xml") {
        return Ok(Some(build_ppt_summary(archive, index)?));
    }
    if index.parts.contains_key("/word/document.xml") {
        return Ok(Some(build_word_summary(archive, index)?));
    }
    if index.parts.contains_key("/xl/workbook.xml") {
        return Ok(Some(build_excel_summary(archive, index)?));
    }
    Ok(None)
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
    let line_number = push_detail_line(&mut view.text, &bounded);
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

fn build_ppt_summary<R: Read + io::Seek>(
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

fn extract_all_text(xml: &[u8]) -> String {
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

fn build_word_summary<R: Read + io::Seek>(
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

fn build_excel_summary<R: Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: &PackageIndex,
) -> io::Result<DetailsView> {
    let workbook = read_part(archive, index, "/xl/workbook.xml")?;
    validate_xml_bytes(&workbook, "/xl/workbook.xml")?;
    let sheets = parse_excel_workbook(&workbook);
    let shared_strings = index
        .parts
        .contains_key("/xl/sharedStrings.xml")
        .then(|| read_part(archive, index, "/xl/sharedStrings.xml"))
        .transpose()?
        .map(|bytes| {
            validate_xml_bytes(&bytes, "/xl/sharedStrings.xml")?;
            Ok::<Vec<String>, io::Error>(parse_shared_strings(&bytes))
        })
        .transpose()?;
    let shared_strings = shared_strings.unwrap_or_default();

    let mut summary = DetailsView {
        text: String::new(),
        links: Vec::new(),
    };
    push_summary_line(&mut summary, "Excel summary", None);
    push_summary_line(&mut summary, "", None);
    push_summary_line(
        &mut summary,
        "Workbook: /xl/workbook.xml",
        Some("/xl/workbook.xml"),
    );
    push_summary_line(&mut summary, format!("Sheets: {}", sheets.len()), None);
    for (name, relationship_id) in sheets {
        push_summary_line(&mut summary, "", None);
        push_summary_line(&mut summary, format!("- {name}"), None);
        let Some(path) = relationship_target_for_id(index, "/xl/workbook.xml", &relationship_id)
        else {
            push_summary_line(&mut summary, "  Worksheet part: unavailable", None);
            continue;
        };
        let worksheet = read_part(archive, index, &path)?;
        validate_xml_bytes(&worksheet, &path)?;
        let sheet_summary = parse_excel_worksheet(&worksheet, &shared_strings);
        push_summary_line(&mut summary, format!("  Worksheet: {path}"), Some(&path));
        push_summary_line(
            &mut summary,
            format!(
                "  Range: {}",
                sheet_summary.range.as_deref().unwrap_or("unknown")
            ),
            None,
        );
        push_summary_line(
            &mut summary,
            format!("  Cells: {}", sheet_summary.cells.len()),
            None,
        );
        push_summary_line(
            &mut summary,
            format!("  Formulas: {}", sheet_summary.formula_count),
            None,
        );
        for cell in sheet_summary.cells.iter().take(20) {
            if let Some(formula) = cell.formula.as_deref() {
                push_summary_line(
                    &mut summary,
                    format!("  {} = {formula}", cell.reference),
                    None,
                );
            } else if !cell.value.is_empty() {
                push_summary_line(
                    &mut summary,
                    format!("  {} = {}", cell.reference, cell.value),
                    None,
                );
            }
        }
        if sheet_summary.cells.len() > 20 {
            push_summary_line(
                &mut summary,
                format!("  ... {} more cells", sheet_summary.cells.len() - 20),
                None,
            );
        }
    }
    Ok(summary)
}

fn parse_excel_workbook(xml: &[u8]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut sheets = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if element_is(event.name().as_ref(), b"sheet") =>
            {
                if let (Some(name), Some(relationship_id)) = (
                    xml_attribute(&event, b"name"),
                    xml_attribute(&event, b"r:id"),
                ) {
                    if sheets.len() < MAX_SUMMARY_ITEMS {
                        sheets.push((name, relationship_id));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    sheets
}

fn parse_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut in_text = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                if element_is(event.name().as_ref(), b"si") {
                    current.clear();
                    in_string = true;
                } else if in_string && element_is(event.name().as_ref(), b"t") {
                    in_text = true;
                }
            }
            Ok(Event::Text(event)) if in_text => {
                append_decoded_text(&mut current, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::GeneralRef(event)) if in_text => {
                append_decoded_reference(&mut current, &event, MAX_SUMMARY_TEXT_CHARS);
            }
            Ok(Event::End(event)) => {
                if element_is(event.name().as_ref(), b"t") {
                    in_text = false;
                } else if element_is(event.name().as_ref(), b"si") {
                    if values.len() < MAX_SHARED_STRINGS {
                        values.push(current.clone());
                    }
                    in_string = false;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }

    values
}

#[derive(Default)]
struct ExcelWorksheetSummary {
    range: Option<String>,
    cells: Vec<ExcelCell>,
    formula_count: usize,
}

#[derive(Default)]
struct ExcelCell {
    reference: String,
    cell_type: Option<String>,
    value: String,
    formula: Option<String>,
}

#[derive(Clone, Copy)]
enum ExcelField {
    Value,
    Formula,
}

fn parse_excel_worksheet(xml: &[u8], shared_strings: &[String]) -> ExcelWorksheetSummary {
    let mut reader = Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut summary = ExcelWorksheetSummary::default();
    let mut current_cell = None;
    let mut active_field = None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"dimension") {
                    summary.range = xml_attribute(&event, b"ref");
                } else if element_is(name.as_ref(), b"c") {
                    current_cell = Some(ExcelCell {
                        reference: xml_attribute(&event, b"r").unwrap_or_default(),
                        cell_type: xml_attribute(&event, b"t"),
                        ..ExcelCell::default()
                    });
                } else if current_cell.is_some() && element_is(name.as_ref(), b"v") {
                    active_field = Some(ExcelField::Value);
                } else if current_cell.is_some() && element_is(name.as_ref(), b"f") {
                    active_field = Some(ExcelField::Formula);
                } else if current_cell.is_some() && element_is(name.as_ref(), b"t") {
                    active_field = Some(ExcelField::Value);
                }
            }
            Ok(Event::Empty(event)) => {
                if element_is(event.name().as_ref(), b"dimension") {
                    summary.range = xml_attribute(&event, b"ref");
                } else if element_is(event.name().as_ref(), b"c")
                    && summary.cells.len() < MAX_SUMMARY_ITEMS
                {
                    summary.cells.push(ExcelCell {
                        reference: xml_attribute(&event, b"r").unwrap_or_default(),
                        cell_type: xml_attribute(&event, b"t"),
                        ..ExcelCell::default()
                    });
                }
            }
            Ok(Event::Text(event)) => {
                if let (Some(cell), Some(field)) = (current_cell.as_mut(), active_field) {
                    match field {
                        ExcelField::Value => {
                            append_decoded_text(&mut cell.value, &event, MAX_SUMMARY_TEXT_CHARS)
                        }
                        ExcelField::Formula => {
                            append_decoded_text(
                                cell.formula.get_or_insert_with(String::new),
                                &event,
                                MAX_SUMMARY_TEXT_CHARS,
                            );
                        }
                    }
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if let (Some(cell), Some(field)) = (current_cell.as_mut(), active_field) {
                    match field {
                        ExcelField::Value => append_decoded_reference(
                            &mut cell.value,
                            &event,
                            MAX_SUMMARY_TEXT_CHARS,
                        ),
                        ExcelField::Formula => append_decoded_reference(
                            cell.formula.get_or_insert_with(String::new),
                            &event,
                            MAX_SUMMARY_TEXT_CHARS,
                        ),
                    }
                }
            }
            Ok(Event::End(event)) => {
                let name = event.name();
                if element_is(name.as_ref(), b"v")
                    || element_is(name.as_ref(), b"f")
                    || element_is(name.as_ref(), b"t")
                {
                    active_field = None;
                } else if element_is(name.as_ref(), b"c") {
                    if let Some(mut cell) = current_cell.take() {
                        if cell.cell_type.as_deref() == Some("s") {
                            if let Ok(index) = cell.value.parse::<usize>() {
                                cell.value = shared_strings.get(index).cloned().unwrap_or_default();
                            }
                        }
                        if cell.formula.is_some() {
                            summary.formula_count += 1;
                        }
                        if summary.cells.len() < MAX_SUMMARY_ITEMS {
                            summary.cells.push(cell);
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

    summary
}

impl App {
    /// Construct an interactive loading state without opening the archive on the UI thread.
    pub fn new_loading(path: String, picker: Picker, worker: Worker) -> io::Result<Self> {
        let app = Self {
            file_path: path.clone(),
            tree_state: TreeState::default(),
            tree_items: Vec::new(),
            editor_state: EditorState::default(),
            image_state: None,
            preview_kind: PreviewKind::Empty,
            picker,
            current_widget: CurrentWidget::Tree,
            status_message: Some("Loading package…".to_string()),
            details_visible: true,
            details_scroll: 0,
            details_cursor: 0,
            package_index: PackageIndex::default(),
            document_summary: None,
            package: None,
            worker,
            open_request_id: 1,
            preview_request_id: 0,
            loading: true,
            worker_error: None,
            #[cfg(test)]
            synchronous: false,
            summary_visible: false,
            summary_scroll: 0,
            navigation_back: Vec::new(),
            navigation_forward: Vec::new(),
            navigation_current: None,
            show_help: false,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: None,
        };
        app.worker.submit(Job::Open {
            request_id: app.open_request_id,
            path: PathBuf::from(&app.file_path),
        })?;
        Ok(app)
    }

    /// Poll without blocking; this is used by the event loop and is deterministic in tests.
    pub fn poll_worker(&mut self) -> bool {
        let mut changed = false;
        loop {
            let result = match self.worker.try_recv() {
                Ok(Some(result)) => result,
                Ok(None) => break,
                Err(error) => {
                    self.loading = false;
                    self.worker_error = Some(error.to_string());
                    self.status_message = Some(format!("Package worker failed: {error}"));
                    return true;
                }
            };
            changed = true;
            match result {
                ResultMessage::Opened {
                    request_id,
                    path,
                    package,
                    summary,
                } => {
                    if request_id != self.open_request_id
                        || path.to_string_lossy() != self.file_path
                    {
                        continue;
                    }
                    match *package {
                        Ok(package) => {
                            self.package_index = package.index.clone();
                            self.package = Some(package);
                            self.editor_state = EditorState::default();
                            self.image_state = None;
                            self.preview_kind = PreviewKind::Empty;
                            self.install_tree();
                            self.document_summary = summary.view;
                            for diagnostic in summary.diagnostics {
                                self.package_index
                                    .warnings
                                    .push(format!("{}: {}", diagnostic.stage, diagnostic.message));
                                self.package_index.diagnostics.push(diagnostic);
                            }
                            self.loading = false;
                            self.status_message = Some(
                                "Select a package part or press Enter to preview content"
                                    .to_string(),
                            );
                        }
                        Err(error) => {
                            self.loading = false;
                            self.worker_error = Some(error.clone());
                            self.status_message = Some(format!("Could not open package: {error}"));
                        }
                    }
                }
                ResultMessage::PartRead {
                    request_id,
                    selected_path,
                    preview,
                } => {
                    let current = self.tree_state.selected().last().cloned();
                    if !current.as_deref().is_some_and(|path| {
                        accepts_result(request_id, self.preview_request_id, &selected_path, path)
                    }) {
                        continue;
                    }
                    match preview {
                        Ok(Preview::Editor { kind, text }) => {
                            self.preview_kind = kind;
                            self.editor_state = EditorState::new(Lines::from(text.as_str()));
                            self.status_message = None;
                        }
                        Ok(Preview::Image(image)) => {
                            self.preview_kind = PreviewKind::Image;
                            self.image_state = Some(self.picker.new_resize_protocol(image));
                            self.status_message = None;
                        }
                        Ok(Preview::Info(message)) => {
                            self.preview_kind = PreviewKind::Info;
                            self.status_message = Some(message);
                        }
                        Ok(Preview::Error(message)) => {
                            self.preview_kind = PreviewKind::Error;
                            self.status_message = Some(format!(
                                "Could not preview {}: {message}",
                                selected_path.trim_start_matches('/')
                            ));
                        }
                        Err(error) => {
                            self.preview_kind = PreviewKind::Error;
                            self.status_message = Some(format!("Could not preview: {error}"));
                        }
                    }
                }
            }
        }
        let worker_busy = self.loading
            || self
                .status_message
                .as_deref()
                .is_some_and(|message| message.starts_with("Loading "));
        if !self.worker.is_alive() && worker_busy {
            self.loading = false;
            let message = "Package worker exited before completing the request".to_string();
            self.worker_error = Some(message.clone());
            self.status_message = Some(message);
            changed = true;
        }
        changed
    }

    pub fn is_package_loaded(&self) -> bool {
        !self.loading && self.package.is_some()
    }

    #[cfg(test)]
    pub fn from_file(path: String, picker: Picker) -> io::Result<Self> {
        let package = Package::open(path.clone())?;
        let package_source = package.source.clone();
        let mut package_index = package.index.clone();
        package_index.source = Some(package_source);
        let file = std::fs::File::open(path.clone())?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut root = Node::new("root", "");
        for normalized_path in package_index
            .parts
            .keys()
            .map(|path| path.trim_start_matches('/'))
        {
            if normalized_path.is_empty() {
                continue;
            }
            let components = normalized_path.split('/').collect::<Vec<&str>>();
            App::build_tree(&mut root, &components, 0);
        }
        let tree_items = App::create_tree(&root)?;
        let document_summary = match build_document_summary(&mut archive, &package_index) {
            Ok(summary) => summary,
            Err(error) => {
                package_index
                    .warnings
                    .push(format!("Could not build document summary: {error}"));
                None
            }
        };

        Ok(Self {
            file_path: path,
            tree_state: TreeState::default(),
            tree_items,
            editor_state: EditorState::default(),
            image_state: None,
            preview_kind: PreviewKind::Empty,
            picker,
            current_widget: CurrentWidget::Tree,
            status_message: Some(
                "Select a package part or press Enter to preview content".to_string(),
            ),
            details_visible: true,
            details_scroll: 0,
            details_cursor: 0,
            package_index,
            document_summary,
            package: Some(package),
            worker: Worker::start()?,
            open_request_id: 0,
            preview_request_id: 0,
            loading: false,
            worker_error: None,
            #[cfg(test)]
            synchronous: true,
            summary_visible: false,
            summary_scroll: 0,
            navigation_back: Vec::new(),
            navigation_forward: Vec::new(),
            navigation_current: None,
            show_help: false,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: None,
        })
    }

    pub fn open_help(&mut self) {
        self.show_help = true;
    }

    pub fn close_help(&mut self) {
        self.show_help = false;
    }

    pub fn toggle_details(&mut self) {
        self.details_visible = !self.details_visible;
        if self.details_visible {
            self.details_scroll = 0;
            self.details_cursor = 0;
        }
    }

    pub fn toggle_summary(&mut self) -> io::Result<()> {
        if self.summary_visible {
            self.summary_visible = false;
            return self.load_selected_file_content_inner(false);
        }

        if self.document_summary.is_none() {
            self.preview_kind = PreviewKind::Error;
            self.status_message = Some("No document-specific summary is available".to_string());
            return Ok(());
        }

        self.summary_visible = true;
        self.summary_scroll = 0;
        self.image_state = None;
        self.editor_state = EditorState::default();
        self.preview_kind = PreviewKind::Summary;
        self.status_message = None;
        Ok(())
    }

    pub fn expand_all(&mut self) {
        fn collect_open_paths(
            items: &[TreeItem<'static, String>],
            parent_path: &[String],
            paths: &mut Vec<Vec<String>>,
        ) {
            for item in items {
                let mut path = parent_path.to_vec();
                path.push(item.identifier().clone());
                if !item.children().is_empty() {
                    paths.push(path.clone());
                    collect_open_paths(item.children(), &path, paths);
                }
            }
        }

        let mut paths = Vec::new();
        collect_open_paths(&self.tree_items, &[], &mut paths);
        for path in paths {
            self.tree_state.open(path);
        }
    }

    pub fn collapse_all(&mut self) {
        self.tree_state.close_all();
        if let Some(first) = self.tree_items.first() {
            self.tree_state.select(vec![first.identifier().clone()]);
        } else {
            self.tree_state.select(Vec::new());
        }
    }

    pub fn details_view(&self) -> DetailsView {
        let Some(selected) = self.tree_state.selected().last() else {
            return DetailsView {
                text: "Select a package part to see metadata\n".to_string(),
                links: Vec::new(),
            };
        };

        let mut text = String::new();
        let mut links = Vec::new();
        let display_name = selected.trim_start_matches('/');
        push_detail_line(&mut text, &format!("Part: {display_name}"));

        if let Some(part) = self.package_index.parts.get(selected) {
            if part.archive_name != display_name {
                push_detail_line(&mut text, &format!("Archive: {}", part.archive_name));
            }
            push_detail_line(&mut text, &format!("Kind: {}", part_kind_label(&part.kind)));
            push_detail_line(
                &mut text,
                &format!(
                    "Content type: {}",
                    part.content_type.as_deref().unwrap_or("Unknown")
                ),
            );
            push_detail_line(
                &mut text,
                &format!(
                    "Size: {} bytes ({} compressed)",
                    part.size, part.compressed_size
                ),
            );
        } else if self.is_directory(selected) {
            push_detail_line(&mut text, "Kind: Directory");
            push_detail_line(&mut text, "Content type: N/A");
        } else {
            push_detail_line(&mut text, "Kind: Unavailable");
            push_detail_line(&mut text, "Content type: Unknown");
        }

        push_detail_line(&mut text, "");
        let outgoing = self.package_index.outgoing.get(selected);
        let incoming = self.package_index.incoming.get(selected);
        push_detail_line(
            &mut text,
            &format!(
                "Relationships: {} outgoing, {} incoming",
                outgoing.map_or(0, Vec::len),
                incoming.map_or(0, Vec::len)
            ),
        );

        if let Some(relationships) = outgoing {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Outgoing");
            for relationship_index in relationships {
                let relationship = &self.package_index.relationships[*relationship_index];
                let target_label = compact_text(&relationship_target_label(relationship), 48);
                let relationship_type = relationship_type_label(relationship);
                let line_number = push_detail_line(
                    &mut text,
                    &format!(
                        "  {}  {} ({relationship_type})",
                        relationship.id, target_label
                    ),
                );
                if let Some(target) = relationship.resolved_target.as_ref() {
                    let start = 2 + relationship.id.chars().count() + 2;
                    links.push(DetailLink {
                        line: line_number,
                        start,
                        end: start + target_label.chars().count(),
                        target: target.clone(),
                    });
                }
            }
        }

        if let Some(relationships) = incoming {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Incoming");
            for relationship_index in relationships {
                let relationship = &self.package_index.relationships[*relationship_index];
                let source = compact_text(&relationship.source, 48);
                let relationship_type = relationship_type_label(relationship);
                let line_number = push_detail_line(
                    &mut text,
                    &format!("  {}  {} ({relationship_type})", relationship.id, source),
                );
                let start = 2 + relationship.id.chars().count() + 2;
                links.push(DetailLink {
                    line: line_number,
                    start,
                    end: start + source.chars().count(),
                    target: relationship.source.clone(),
                });
            }
        }

        if !self.package_index.warnings.is_empty() {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Warnings");
            for warning in &self.package_index.warnings {
                push_detail_line(&mut text, &format!("- {}", compact_text(warning, 56)));
            }
        }

        DetailsView { text, links }
    }

    pub fn scroll_details(&mut self, amount: i16) {
        if amount.is_negative() {
            self.details_scroll = self.details_scroll.saturating_sub(amount.unsigned_abs());
        } else {
            self.details_scroll = self.details_scroll.saturating_add(amount as u16);
        }
    }

    pub fn move_details_cursor(&mut self, reverse: bool) {
        let links = self.details_view().links;
        if links.is_empty() {
            self.scroll_details(if reverse { -1 } else { 1 });
            return;
        }

        self.details_cursor = if reverse {
            self.details_cursor
                .checked_sub(1)
                .unwrap_or(links.len() - 1)
        } else {
            (self.details_cursor + 1) % links.len()
        };
        let line = links[self.details_cursor].line;
        self.details_scroll = line.saturating_sub(2) as u16;
    }

    pub fn activate_current_detail_link(&mut self) -> io::Result<bool> {
        let Some((line, start)) = self
            .details_view()
            .links
            .get(self.details_cursor)
            .map(|link| (link.line, link.start))
        else {
            return Ok(false);
        };
        self.activate_detail_link(line, start)
    }

    pub fn activate_detail_link(&mut self, line: usize, column: usize) -> io::Result<bool> {
        let Some(link) = self
            .details_view()
            .links
            .into_iter()
            .find(|link| link.line == line && column >= link.start && column < link.end)
        else {
            return Ok(false);
        };
        let target = link.target;

        if !self.package_index.parts.contains_key(&target) && !self.is_directory(&target) {
            return Ok(false);
        }
        self.select_path(&target);
        self.details_scroll = 0;
        self.load_selected_file_content()?;
        Ok(true)
    }

    pub fn scroll_summary(&mut self, amount: i16) {
        if amount.is_negative() {
            self.summary_scroll = self.summary_scroll.saturating_sub(amount.unsigned_abs());
        } else {
            self.summary_scroll = self.summary_scroll.saturating_add(amount as u16);
        }
    }

    pub fn activate_summary_link(&mut self, line: usize, column: usize) -> io::Result<bool> {
        let Some(summary) = self.document_summary.as_ref() else {
            return Ok(false);
        };
        let Some(target) = summary
            .links
            .iter()
            .find(|link| link.line == line && column >= link.start && column < link.end)
            .map(|link| link.target.clone())
        else {
            return Ok(false);
        };

        if !self.package_index.parts.contains_key(&target) && !self.is_directory(&target) {
            return Ok(false);
        }
        self.summary_visible = false;
        self.summary_scroll = 0;
        self.select_path(&target);
        self.load_selected_file_content_inner(true)?;
        Ok(true)
    }

    pub fn start_search(&mut self) {
        self.search_active = true;
        if self
            .status_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.status_message = None;
        }
        self.search_query.clear();
        self.search_matches.clear();
        self.search_index = None;
    }

    pub fn search_input_char(&mut self, character: char) {
        self.search_query.push(character);
    }

    pub fn search_backspace(&mut self) {
        self.search_query.pop();
    }

    pub fn finish_search(&mut self) {
        self.search_active = false;
        self.update_search_matches();
    }

    pub fn cancel_search(&mut self) {
        self.search_active = false;
        if self
            .status_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.status_message = None;
        }
        self.search_query.clear();
        self.search_matches.clear();
        self.search_index = None;
    }

    pub fn next_search_match(&mut self, reverse: bool) {
        if self.search_matches.is_empty() {
            self.update_search_matches();
        }
        if self.search_matches.is_empty() {
            return;
        }

        let current = self.search_index.unwrap_or(0);
        let next = if reverse {
            if current == 0 {
                self.search_matches.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % self.search_matches.len()
        };
        self.search_index = Some(next);
        self.select_path(&self.search_matches[next].clone());
    }

    pub fn selection_status(&self) -> String {
        let Some(selected) = self.tree_state.selected().last() else {
            return if self.search_active {
                format!("Search: {}_ | Enter select, Esc cancel", self.search_query)
            } else {
                "No package part selected".to_string()
            };
        };

        let display_name = selected.trim_start_matches('/');
        let part_type = if self.is_directory(selected) {
            "Directory"
        } else if Self::is_xml(display_name) {
            "XML"
        } else if Self::is_image(display_name) {
            "Image"
        } else {
            "Binary/unsupported"
        };

        let mut status = format!("Part: {display_name} | Type: {part_type}");
        if self.search_active {
            status.push_str(&format!(" | Search: {}", self.search_query));
        } else if !self.search_query.is_empty() {
            status.push_str(&format!(" | Search: {} (n/N next)", self.search_query));
        }
        status
    }

    fn record_navigation(&mut self, selected: &str) {
        if self.navigation_current.as_deref() == Some(selected) {
            return;
        }
        if let Some(current) = self.navigation_current.replace(selected.to_string()) {
            self.navigation_back.push(current);
        }
        self.navigation_forward.clear();
    }

    pub fn navigate_back(&mut self) -> io::Result<bool> {
        let Some(previous) = self.navigation_back.pop() else {
            return Ok(false);
        };
        if let Some(current) = self.navigation_current.clone() {
            self.navigation_forward.push(current);
        }
        self.select_path(&previous);
        self.load_selected_file_content_inner(false)?;
        self.navigation_current = Some(previous);
        Ok(true)
    }

    pub fn navigate_forward(&mut self) -> io::Result<bool> {
        let Some(next) = self.navigation_forward.pop() else {
            return Ok(false);
        };
        if let Some(current) = self.navigation_current.clone() {
            self.navigation_back.push(current);
        }
        self.select_path(&next);
        self.load_selected_file_content_inner(false)?;
        self.navigation_current = Some(next);
        Ok(true)
    }

    pub fn load_selected_file_content(&mut self) -> io::Result<()> {
        self.load_selected_file_content_inner(true)
    }

    fn load_selected_file_content_inner(&mut self, record_history: bool) -> io::Result<()> {
        // Reset the previous view before loading anything new. This prevents a
        // previously selected XML file or image from remaining visible when a
        // directory or unsupported package part is selected.
        self.image_state = None;
        self.editor_state = EditorState::default();
        self.preview_kind = PreviewKind::Empty;
        self.summary_visible = false;
        self.summary_scroll = 0;
        self.status_message = Some("Select a package part to inspect".to_string());

        let selected = match self.tree_state.selected().last().cloned() {
            Some(selected) => selected,
            None => return Ok(()),
        };
        if record_history {
            self.record_navigation(&selected);
        }

        let display_name = selected.trim_start_matches('/');
        let Some(part) = self.package_index.parts.get(&selected) else {
            if self.is_directory(&selected) {
                self.status_message = Some(format!("Directory: {display_name}"));
            } else {
                self.status_message = Some(format!("Unavailable package part: {display_name}"));
            }
            return Ok(());
        };
        if part.kind == PartKind::Directory {
            self.status_message = Some(format!("Directory: {display_name}"));
            return Ok(());
        }
        let Some(package) = self.package.as_ref() else {
            self.status_message = Some("Package is still loading".to_string());
            return Ok(());
        };
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
        if let Err(error) = self.worker.submit(Job::ReadPart {
            request_id: self.preview_request_id,
            package_path: package.source.clone(),
            selected_path: selected.clone(),
            archive_name: part.archive_name.clone(),
            content_type: part.content_type.clone(),
            size: part.size,
            compressed_size: part.compressed_size,
            index: Box::new(self.package_index.clone()),
        }) {
            self.preview_kind = PreviewKind::Error;
            self.worker_error = Some(error.to_string());
            self.status_message = Some(format!("Package worker failed: {error}"));
            return Ok(());
        }
        self.status_message = Some(format!("Loading {display_name}…"));
        #[cfg(test)]
        if self.synchronous {
            for _ in 0..200 {
                if self.poll_worker() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Ok(())
    }

    fn is_directory(&self, path: &str) -> bool {
        self.package_index.is_directory(path)
    }

    fn install_tree(&mut self) {
        let mut root = Node::new("root", "");
        for normalized_path in self
            .package_index
            .parts
            .keys()
            .map(|path| path.trim_start_matches('/'))
        {
            if normalized_path.is_empty() {
                continue;
            }
            let components = normalized_path.split('/').collect::<Vec<&str>>();
            Self::build_tree(&mut root, &components, 0);
        }
        match Self::create_tree(&root) {
            Ok(tree_items) => self.tree_items = tree_items,
            Err(error) => {
                self.tree_items.clear();
                self.worker_error = Some(format!("Could not build package tree: {error}"));
            }
        }
    }

    fn select_path(&mut self, path: &str) {
        let mut identifiers = Vec::new();
        let mut current = String::new();
        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() {
                continue;
            }
            current.push('/');
            current.push_str(component);
            identifiers.push(current.clone());
        }
        for index in 0..identifiers.len().saturating_sub(1) {
            self.tree_state.open(identifiers[..=index].to_vec());
        }
        self.tree_state.select(identifiers);
        self.tree_state.scroll_selected_into_view();
    }

    fn update_search_matches(&mut self) {
        let query = self.search_query.to_ascii_lowercase();
        if query.is_empty() {
            self.search_matches.clear();
            self.search_index = None;
            return;
        }

        self.search_matches = self
            .package_index
            .parts
            .keys()
            .filter(|path| path.to_ascii_lowercase().contains(&query))
            .cloned()
            .collect();
        self.search_matches.sort();

        if self.search_matches.is_empty() {
            self.search_index = None;
            self.status_message = Some(format!("No package parts match: {}", self.search_query));
            return;
        }

        self.search_index = Some(0);
        let path = self.search_matches[0].clone();
        self.select_path(&path);
    }

    fn is_xml(path: &str) -> bool {
        matches!(
            std::path::Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref(),
            Some("xml" | "rels")
        )
    }

    fn is_image(path: &str) -> bool {
        matches!(
            std::path::Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp")
        )
    }

    fn image_format(path: &str) -> ImageFormat {
        match std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg" | "jpeg") => ImageFormat::Jpeg,
            Some("gif") => ImageFormat::Gif,
            Some("bmp") => ImageFormat::Bmp,
            Some("webp") => ImageFormat::WebP,
            _ => ImageFormat::Png,
        }
    }

    fn pretty_print_xml(xml: &str) -> io::Result<String> {
        struct LimitedWriter {
            output: Vec<u8>,
            limit: usize,
        }

        impl io::Write for LimitedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if bytes.len() > self.limit.saturating_sub(self.output.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("formatted XML preview exceeds {MAX_XML_PREVIEW_BYTES} byte limit"),
                    ));
                }
                self.output.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);

        let mut output = LimitedWriter {
            output: Vec::with_capacity(xml.len().min(MAX_XML_PREVIEW_BYTES)),
            limit: MAX_XML_PREVIEW_BYTES,
        };
        let mut writer = Writer::new_with_indent(&mut output, b' ', 2);
        let mut buffer = Vec::new();

        loop {
            match reader.read_event_into(&mut buffer) {
                Ok(Event::Eof) => break,
                Ok(event) => writer.write_event(event).map_err(io::Error::other)?,
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            }
            buffer.clear();
        }

        String::from_utf8(output.output).map_err(io::Error::other)
    }

    fn build_tree(node: &mut Node, parts: &[&str], depth: usize) {
        if depth >= MAX_PATH_DEPTH || depth >= parts.len() {
            return;
        }
        let item = parts[depth];
        let child_index = match node.children.iter().position(|child| child.name == item) {
            Some(index) => index,
            None => {
                let path = format!("{}/{}", node.path, item);
                node.children.push(Node::new(item, &path));
                node.children.len() - 1
            }
        };
        App::build_tree(&mut node.children[child_index], parts, depth + 1);
    }

    fn create_tree(root: &Node) -> io::Result<Vec<TreeItem<'static, String>>> {
        fn to_tree_item(node: &Node) -> io::Result<TreeItem<'static, String>> {
            let text = node.name.to_owned();
            let identifier = node.path.to_owned();

            if node.children.is_empty() {
                Ok(TreeItem::new_leaf(identifier, text))
            } else {
                TreeItem::new(identifier, text, parse_children(node)?).map_err(io::Error::other)
            }
        }
        fn parse_children(node: &Node) -> io::Result<Vec<TreeItem<'static, String>>> {
            node.children.iter().map(to_tree_item).collect()
        }

        parse_children(root)
    }
}

#[cfg(test)]
mod tests {
    use crate::package::normalize_package_path;
    use crate::{worker::Worker, App};
    use ratatui_image::picker::Picker;
    use std::io;

    #[test]
    fn loading_constructor_installs_worker_package_result() -> io::Result<()> {
        let worker = Worker::start()?;
        let mut app =
            App::new_loading("data/sample.pptx".to_string(), Picker::halfblocks(), worker)?;
        assert!(app.loading);
        assert!(app.tree_items.is_empty());
        for _ in 0..200 {
            if app.poll_worker() && !app.loading {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.loading);
        assert!(app.is_package_loaded());
        assert!(!app.tree_items.is_empty());
        assert!(app.document_summary.is_some());
        Ok(())
    }

    #[test]
    fn loading_error_keeps_no_package_state() -> io::Result<()> {
        let worker = Worker::start()?;
        let mut app = App::new_loading(
            "/definitely/not/a/package.pptx".to_string(),
            Picker::halfblocks(),
            worker,
        )?;
        for _ in 0..200 {
            if app.poll_worker() && !app.loading {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.loading);
        assert!(!app.is_package_loaded());
        assert!(app.tree_items.is_empty());
        assert!(app.selection_status().contains("No package"));
        app.expand_all();
        app.collapse_all();
        Ok(())
    }

    #[test]
    fn load_pptx() -> io::Result<()> {
        let app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        assert!(!app.tree_items.is_empty());
        let summary = app
            .document_summary
            .as_ref()
            .expect("sample presentation should have a summary");
        assert!(summary.text.contains("Slides: 2"));
        assert!(summary.text.contains("OOXML TUI"));
        assert!(summary
            .links
            .iter()
            .any(|link| link.target == "/ppt/slides/slide1.xml"));

        Ok(())
    }

    #[test]
    fn summary_links_navigate_to_package_parts() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        app.toggle_summary()?;
        let link = app
            .document_summary
            .as_ref()
            .and_then(|summary| {
                summary
                    .links
                    .iter()
                    .find(|link| link.target == "/ppt/slides/slide1.xml")
            })
            .cloned()
            .expect("summary should link to the first slide");
        assert!(app.activate_summary_link(link.line, link.start)?);
        assert!(!app.summary_visible);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/slides/slide1.xml")
        );
        assert_eq!(app.preview_kind, super::PreviewKind::Xml);
        Ok(())
    }

    #[test]
    fn toggle_document_summary_switches_back_to_selected_content() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        app.toggle_summary()?;
        assert_eq!(app.preview_kind, super::PreviewKind::Summary);
        assert!(app.summary_visible);

        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.toggle_summary()?;
        assert!(!app.summary_visible);
        assert_eq!(app.preview_kind, super::PreviewKind::Xml);
        assert!(app.status_message.is_none());
        Ok(())
    }

    #[test]
    fn load_selected_file_content() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        assert!(app.details_visible);
        app.tree_state
            .select(vec!["/ppt/media/image1.gif".to_string()]);
        app.load_selected_file_content()?;
        assert!(app.image_state.is_some());
        assert!(app.status_message.is_none());

        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.load_selected_file_content()?;
        assert!(app.image_state.is_none());
        assert!(app.status_message.is_none());

        app.tree_state.select(vec!["/ppt".to_string()]);
        app.load_selected_file_content()?;
        assert!(app.image_state.is_none());
        assert_eq!(app.status_message.as_deref(), Some("Directory: ppt"));

        Ok(())
    }

    #[test]
    fn malformed_xml_is_reported_as_an_error() {
        assert!(App::pretty_print_xml("<root><item></root>").is_err());
        let error = super::validate_xml_bytes(b"<root><item></root>", "/ppt/presentation.xml")
            .expect_err("summary validation must reject malformed XML");
        assert!(error.to_string().contains("/ppt/presentation.xml"));
        assert!(
            super::validate_xml_bytes(b"<sst><si><t>bad</sst>", "/xl/sharedStrings.xml").is_err()
        );
    }

    #[test]
    fn xml_preview_rejects_indentation_amplification() {
        let depth = super::MAX_XML_PREVIEW_BYTES / 1024;
        let mut xml = String::from("<root>");
        for _ in 0..depth {
            xml.push_str("<item>");
        }
        for _ in 0..depth {
            xml.push_str("</item>");
        }
        xml.push_str("</root>");

        let error = App::pretty_print_xml(&xml).expect_err("formatted XML must be bounded");
        assert!(error.to_string().contains("formatted XML preview exceeds"));
    }

    #[test]
    fn summary_text_decodes_entities_and_stays_bounded() {
        let text = super::extract_all_text(br#"<root>one &amp; &lt;two&gt;</root>"#);
        assert_eq!(text, "one & <two>");

        let mut view = super::DetailsView {
            text: String::new(),
            links: Vec::new(),
        };
        for _ in 0..(super::MAX_SUMMARY_LINES * 2) {
            super::push_summary_line(&mut view, "summary line", None);
        }
        assert!(view.text.len() <= super::MAX_SUMMARY_CHARS);
        assert!(view.text.lines().count() <= super::MAX_SUMMARY_LINES);
    }

    #[test]
    fn preview_factory_formats_json_and_text() {
        let json = super::build_preview(
            "custom.json",
            Some("application/json"),
            13,
            9,
            br#"{"answer":42}"#,
        );
        match json {
            super::Preview::Editor { kind, text } => {
                assert_eq!(kind, super::PreviewKind::Json);
                assert!(text.contains("  \"answer\": 42"));
            }
            other => panic!("expected JSON editor preview, got {other:?}"),
        }

        let text = super::build_preview("notes.txt", None, 11, 7, b"hello\nworld");
        assert!(matches!(
            text,
            super::Preview::Editor {
                kind: super::PreviewKind::PlainText,
                ..
            }
        ));
    }

    #[test]
    fn document_summary_parsers_extract_word_and_excel_details() {
        let word = br#"
            <w:document xmlns:w="word">
              <w:body>
                <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p>
                <w:p><w:r><w:t>Body text</w:t></w:r></w:p>
                <w:tbl/>
              </w:body>
            </w:document>
        "#;
        let (paragraphs, tables, headings) = super::parse_word_document(word);
        assert_eq!(paragraphs, 2);
        assert_eq!(tables, 1);
        assert_eq!(headings, vec![(1, "Overview".to_string())]);

        let excel = br#"
            <worksheet xmlns:r="relationships">
              <dimension ref="A1:B2"/>
              <sheetData>
                <row>
                  <c r="A1" t="inlineStr"><is><t>Hello</t></is></c>
                  <c r="B1"><f>SUM(A2:A3)</f><v>3</v></c>
                </row>
              </sheetData>
            </worksheet>
        "#;
        let worksheet = super::parse_excel_worksheet(excel, &[]);
        assert_eq!(worksheet.range.as_deref(), Some("A1:B2"));
        assert_eq!(worksheet.cells.len(), 2);
        assert_eq!(worksheet.formula_count, 1);
        assert_eq!(worksheet.cells[0].value, "Hello");
        assert_eq!(worksheet.cells[1].formula.as_deref(), Some("SUM(A2:A3)"));
    }

    #[test]
    fn preview_factory_formats_hex_and_binary_information() {
        let hex = super::build_preview("payload.bin", None, 3, 5, &[0, 1, b'A']);
        match hex {
            super::Preview::Editor { kind, text } => {
                assert_eq!(kind, super::PreviewKind::Hex);
                assert!(text.contains("00000000"));
                assert!(text.contains("00 01 41"));
                assert!(text.contains("|..A|"));
            }
            other => panic!("expected hex preview, got {other:?}"),
        }

        let font = super::build_preview("font.ttf", None, 100, 50, &[0, 1, 2]);
        match font {
            super::Preview::Info(message) => {
                assert!(message.contains("Category:   Font"));
                assert!(message.contains("MIME type:  font/ttf"));
            }
            other => panic!("expected binary information preview, got {other:?}"),
        }

        let ole = super::build_preview(
            "embeddings/object.bin",
            Some("application/vnd.ms-office.vbaProject"),
            100,
            50,
            &[0, 1, 2],
        );
        assert!(matches!(ole, super::Preview::Info(message) if message.contains("OLE/VBA object")));
    }

    #[test]
    fn unusual_paths_are_normalized_for_tree_identifiers() {
        assert_eq!(
            normalize_package_path("/ppt//slides/./slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(normalize_package_path("///"), "");
    }

    #[test]
    fn file_type_detection_is_case_insensitive() {
        assert!(App::is_xml("custom.XML"));
        assert!(App::is_image("media/PHOTO.JpEg"));
        assert_eq!(
            App::image_format("media/PHOTO.JpEg"),
            image::ImageFormat::Jpeg
        );
    }

    #[test]
    fn expand_and_collapse_all_tree_nodes() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        app.expand_all();
        assert!(!app.tree_state.opened().is_empty());

        app.collapse_all();
        assert!(app.tree_state.opened().is_empty());
        assert_eq!(app.tree_state.selected().len(), 1);

        Ok(())
    }

    #[test]
    fn navigation_history_moves_back_and_forward() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.load_selected_file_content()?;
        app.tree_state.select(vec![
            "/ppt".to_string(),
            "/ppt/presentation.xml".to_string(),
        ]);
        app.load_selected_file_content()?;

        assert!(app.navigate_back()?);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/[Content_Types].xml")
        );
        assert!(app.navigate_forward()?);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/presentation.xml")
        );

        Ok(())
    }

    #[test]
    fn package_metadata_and_relationships_are_indexed() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        let slide = app
            .package_index
            .parts
            .get("/ppt/slides/slide1.xml")
            .expect("sample slide should be indexed");
        assert_eq!(slide.kind, super::PartKind::Xml);
        assert!(slide
            .content_type
            .as_deref()
            .is_some_and(|content_type| { content_type.contains("presentationml.slide+xml") }));

        let outgoing = app
            .package_index
            .outgoing
            .get("/ppt/slides/slide1.xml")
            .expect("sample slide should have relationships");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(
            app.package_index.relationships[outgoing[0]]
                .resolved_target
                .as_deref(),
            Some("/ppt/slideLayouts/slideLayout1.xml")
        );

        app.tree_state.select(vec![
            "/ppt".to_string(),
            "/ppt/slides".to_string(),
            "/ppt/slides/slide1.xml".to_string(),
        ]);
        let details = app.details_view();
        assert!(!details.links.is_empty());
        app.activate_detail_link(details.links[0].line, details.links[0].start)?;
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/slideLayouts/slideLayout1.xml")
        );

        Ok(())
    }

    #[test]
    fn search_selects_matching_package_parts() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        app.start_search();
        for character in "/ppt/slides/slide1.xml".chars() {
            app.search_input_char(character);
        }
        app.finish_search();

        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/slides/slide1.xml")
        );
        assert_eq!(app.search_matches.len(), 1);
        assert!(app.selection_status().contains("Type: XML"));

        Ok(())
    }
}
