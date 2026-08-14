//! Bounded part-preview classification and formatting.
//!
//! Extension-based classification lives in `crate::package` (`is_xml_name`,
//! `is_image_name`, `image_format`) and is reused here; this module adds
//! content-type-aware detection and the preview renderers themselves.
use std::{
    fmt::Write as _,
    io::{self, Cursor},
};

use image::{DynamicImage, ImageReader, Limits};
use quick_xml::{events::Event, Reader, Writer};

use crate::package::{image_format, is_image_name, is_xml_content_type, is_xml_name};

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
pub enum Preview {
    Editor { kind: PreviewKind, text: String },
    Image(DynamicImage),
    Info(String),
    Error(String),
}

pub const MAX_HEX_PREVIEW_BYTES: usize = 1024 * 1024;
pub const MAX_XML_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_JSON_PREVIEW_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 8192;
pub const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

/// A writer that fails once the output exceeds a fixed byte limit, protecting the
/// UI from formatting amplification on hostile input.
pub(crate) struct LimitedWriter {
    output: Vec<u8>,
    limit: usize,
    label: &'static str,
}

impl LimitedWriter {
    pub(crate) fn new(capacity: usize, limit: usize, label: &'static str) -> Self {
        Self {
            output: Vec::with_capacity(capacity),
            limit,
            label,
        }
    }

    fn into_string(self) -> io::Result<String> {
        String::from_utf8(self.output).map_err(io::Error::other)
    }
}

impl io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.output.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} exceeds {} byte limit", self.label, self.limit),
            ));
        }
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn build_preview(
    path: &str,
    content_type: Option<&str>,
    size: u64,
    compressed_size: u64,
    bytes: &[u8],
) -> Preview {
    if is_image_name(path) {
        let mut reader = ImageReader::with_format(Cursor::new(bytes), image_format(path));
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

    if is_xml_name(path) || content_type.is_some_and(is_xml_content_type) {
        let text = String::from_utf8_lossy(bytes);
        return match pretty_print_xml(&text) {
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

pub fn pretty_print_xml(xml: &str) -> io::Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut output = LimitedWriter::new(
        xml.len().min(MAX_XML_PREVIEW_BYTES),
        MAX_XML_PREVIEW_BYTES,
        "formatted XML preview",
    );
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

    output.into_string()
}

fn pretty_print_json(value: &serde_json::Value) -> io::Result<String> {
    let mut output = LimitedWriter::new(0, MAX_JSON_PREVIEW_BYTES, "formatted JSON preview");
    serde_json::to_writer_pretty(&mut output, value).map_err(io::Error::other)?;
    output.into_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_xml_is_reported_as_an_error() {
        assert!(pretty_print_xml("<root><item></root>").is_err());
    }

    #[test]
    fn xml_preview_rejects_indentation_amplification() {
        let depth = MAX_XML_PREVIEW_BYTES / 1024;
        let mut xml = String::from("<root>");
        for _ in 0..depth {
            xml.push_str("<item>");
        }
        for _ in 0..depth {
            xml.push_str("</item>");
        }
        xml.push_str("</root>");

        let error = pretty_print_xml(&xml).expect_err("formatted XML must be bounded");
        assert!(error.to_string().contains("formatted XML preview exceeds"));
    }

    #[test]
    fn preview_factory_formats_json_and_text() {
        let json = build_preview(
            "custom.json",
            Some("application/json"),
            13,
            9,
            br#"{"answer":42}"#,
        );
        match json {
            Preview::Editor { kind, text } => {
                assert_eq!(kind, PreviewKind::Json);
                assert!(text.contains("  \"answer\": 42"));
            }
            other => panic!("expected JSON editor preview, got {other:?}"),
        }

        let text = build_preview("notes.txt", None, 11, 7, b"hello\nworld");
        assert!(matches!(
            text,
            Preview::Editor {
                kind: PreviewKind::PlainText,
                ..
            }
        ));
    }

    #[test]
    fn preview_factory_formats_hex_and_binary_information() {
        let hex = build_preview("payload.bin", None, 3, 5, &[0, 1, b'A']);
        match hex {
            Preview::Editor { kind, text } => {
                assert_eq!(kind, PreviewKind::Hex);
                assert!(text.contains("00000000"));
                assert!(text.contains("00 01 41"));
                assert!(text.contains("|..A|"));
            }
            other => panic!("expected hex preview, got {other:?}"),
        }

        let font = build_preview("font.ttf", None, 100, 50, &[0, 1, 2]);
        match font {
            Preview::Info(message) => {
                assert!(message.contains("Category:   Font"));
                assert!(message.contains("MIME type:  font/ttf"));
            }
            other => panic!("expected binary information preview, got {other:?}"),
        }

        let ole = build_preview(
            "embeddings/object.bin",
            Some("application/vnd.ms-office.vbaProject"),
            100,
            50,
            &[0, 1, 2],
        );
        assert!(matches!(ole, Preview::Info(message) if message.contains("OLE/VBA object")));
    }

    #[test]
    fn xml_is_detected_from_content_type_for_unusual_extensions() {
        let preview = build_preview(
            "customXml/item1.data",
            Some("application/vnd.openxmlformats-officedocument.customXmlProperties+xml"),
            20,
            12,
            b"<root><a/></root>",
        );
        assert!(matches!(
            preview,
            Preview::Editor {
                kind: PreviewKind::Xml,
                ..
            }
        ));
    }
}
