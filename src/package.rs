//! Canonical OOXML package metadata and bounded archive access.
use std::{
    collections::BTreeMap,
    io::{self, Read, Seek},
    path::{Path, PathBuf},
    sync::Arc,
};

use image::ImageFormat;
use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};

pub const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_ENTRY_COUNT: usize = 100_000;
pub const MAX_PATH_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub stage: String,
    pub part: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(
        stage: impl Into<String>,
        part: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            stage: stage.into(),
            part,
            message: message.into(),
        }
    }
    pub fn error(
        stage: impl Into<String>,
        part: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            stage: stage.into(),
            part,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartKind {
    Xml,
    Image,
    Binary,
    Directory,
}

#[derive(Clone, Debug)]
pub struct PartInfo {
    pub path: String,
    pub archive_name: String,
    pub content_type: Option<String>,
    pub size: u64,
    pub compressed_size: u64,
    pub kind: PartKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetMode {
    Internal,
    External,
}

#[derive(Clone, Debug)]
pub struct Relationship {
    pub source: String,
    pub id: String,
    pub relationship_type: String,
    pub target: String,
    pub resolved_target: Option<String>,
    pub target_mode: TargetMode,
}

#[derive(Clone, Debug, Default)]
struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct PackageIndex {
    pub parts: BTreeMap<String, PartInfo>,
    pub relationships: Vec<Relationship>,
    pub outgoing: BTreeMap<String, Vec<usize>>,
    pub incoming: BTreeMap<String, Vec<usize>>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub source: Option<PathBuf>,
    content_types: ContentTypes,
}

impl PackageIndex {
    pub fn from_archive<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> io::Result<Self> {
        let mut index = Self::default();
        let mut content_types_bytes = None;
        let mut relationship_parts = Vec::new();
        let mut total = 0u64;
        let entry_count = archive.len();
        check_archive_limits(entry_count, 0)?;
        for entry_index in 0..entry_count {
            let mut entry = archive.by_index(entry_index).map_err(io::Error::other)?;
            let archive_name = entry.name().to_string();
            let size = entry.size();
            let compressed_size = entry.compressed_size();
            total = total.saturating_add(size);
            if let Err(error) = check_archive_limits(entry_count, total) {
                index.record(Diagnostic::error("index", None, error.to_string()));
                return Err(error);
            }
            let normalized_path = normalize_package_path(&archive_name);
            let unsafe_name = archive_name.split('/').any(|part| part == "..")
                || archive_name.starts_with('/')
                || archive_name.contains('\\');
            if unsafe_name {
                index.record(Diagnostic::warning(
                    "index",
                    Some(archive_name.clone()),
                    "traversal-like archive name was normalized",
                ));
            }
            if normalized_path.is_empty() {
                continue;
            }
            if normalized_path.split('/').count() > MAX_PATH_DEPTH {
                index.record(Diagnostic::warning(
                    "index",
                    Some(archive_name),
                    format!(
                        "archive path exceeds maximum depth of {MAX_PATH_DEPTH}; entry ignored"
                    ),
                ));
                continue;
            }
            let path = format!("/{normalized_path}");
            if index.parts.contains_key(&path) {
                index.record(Diagnostic::error(
                    "index",
                    Some(path.clone()),
                    format!(
                        "archive path collides with '{}'; entry ignored",
                        archive_name
                    ),
                ));
                continue;
            }
            let is_directory = entry.is_dir();
            let kind = if is_directory {
                PartKind::Directory
            } else if is_xml_name(&archive_name) {
                PartKind::Xml
            } else if is_image_name(&archive_name) {
                PartKind::Image
            } else {
                PartKind::Binary
            };
            index.parts.insert(
                path.clone(),
                PartInfo {
                    path: path.clone(),
                    archive_name: archive_name.clone(),
                    content_type: None,
                    size,
                    compressed_size,
                    kind,
                },
            );
            if normalized_path.eq_ignore_ascii_case("[Content_Types].xml") {
                match read_limited(&mut entry, MAX_METADATA_BYTES, size) {
                    Ok(bytes) => content_types_bytes = Some(bytes),
                    Err(error) => index.record(Diagnostic::error(
                        "content-types",
                        Some(path),
                        error.to_string(),
                    )),
                }
            } else if archive_name.to_ascii_lowercase().ends_with(".rels") {
                match read_limited(&mut entry, MAX_METADATA_BYTES, size) {
                    Ok(bytes) => relationship_parts.push((normalized_path, bytes)),
                    Err(error) => index.record(Diagnostic::error(
                        "relationships",
                        Some(path),
                        error.to_string(),
                    )),
                }
            }
        }
        if let Some(bytes) = content_types_bytes {
            match parse_content_types(&bytes) {
                Ok(content_types) => index.content_types = content_types,
                Err(error) => index.record(Diagnostic::error(
                    "content-types",
                    Some("/[Content_Types].xml".into()),
                    error.to_string(),
                )),
            }
        }
        for part in index.parts.values_mut() {
            let content_type = index.content_types.content_type_for(&part.path);
            // Refine extension-based classification with the declared content type so
            // XML or image parts with unusual extensions still preview correctly.
            if part.kind == PartKind::Binary {
                if content_type.as_deref().is_some_and(is_xml_content_type) {
                    part.kind = PartKind::Xml;
                } else if content_type
                    .as_deref()
                    .is_some_and(|value| value.to_ascii_lowercase().starts_with("image/"))
                {
                    part.kind = PartKind::Image;
                }
            }
            part.content_type = content_type;
        }
        for (relationship_path, bytes) in relationship_parts {
            let Some(source) = relationship_source(&relationship_path) else {
                index.record(Diagnostic::warning(
                    "relationships",
                    Some(format!("/{relationship_path}")),
                    "could not determine relationship source",
                ));
                continue;
            };
            match parse_relationships(&bytes, &source) {
                Ok(mut relationships) => index.relationships.append(&mut relationships),
                Err(error) => index.record(Diagnostic::error(
                    "relationships",
                    Some(format!("/{relationship_path}")),
                    error.to_string(),
                )),
            }
        }
        for (relationship_index, relationship) in index.relationships.iter().enumerate() {
            index
                .outgoing
                .entry(relationship.source.clone())
                .or_default()
                .push(relationship_index);
            if let Some(target) = relationship.resolved_target.as_ref() {
                index
                    .incoming
                    .entry(target.clone())
                    .or_default()
                    .push(relationship_index);
            }
        }
        Ok(index)
    }

    pub(crate) fn record(&mut self, diagnostic: Diagnostic) {
        self.warnings
            .push(format!("{}: {}", diagnostic.stage, diagnostic.message));
        self.diagnostics.push(diagnostic);
    }

    pub fn read_part<R: Read + Seek>(
        &self,
        archive: &mut zip::ZipArchive<R>,
        path: &str,
        limit: u64,
    ) -> io::Result<Vec<u8>> {
        let part = self.parts.get(path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("part not found: {path}"))
        })?;
        let mut entry = archive
            .by_name(&part.archive_name)
            .map_err(io::Error::other)?;
        let declared = entry.size();
        let effective_limit = limit.min(MAX_ENTRY_BYTES);
        read_limited(&mut entry, effective_limit, declared)
    }

    pub fn is_directory(&self, path: &str) -> bool {
        self.parts
            .get(path)
            .is_some_and(|part| part.kind == PartKind::Directory)
            || self
                .parts
                .keys()
                .any(|child| child.starts_with(&format!("{path}/")))
    }
}

/// A package owns the canonical source path and immutable metadata snapshot.
/// The index is shared with background workers via `Arc` to avoid cloning the
/// full metadata graph for every preview request.
#[derive(Clone, Debug)]
pub struct Package {
    pub source: PathBuf,
    pub index: Arc<PackageIndex>,
}

impl Package {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let source = path.into();
        let file = std::fs::File::open(&source)?;
        let mut index = PackageIndex::from_archive(&mut zip::ZipArchive::new(file)?)?;
        index.source = Some(source.clone());
        Ok(Self {
            source,
            index: Arc::new(index),
        })
    }
}

fn check_archive_limits(entry_count: usize, declared_total: u64) -> io::Result<()> {
    if entry_count > MAX_ENTRY_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("archive has {entry_count} entries; limit is {MAX_ENTRY_COUNT}"),
        ));
    }
    if declared_total > MAX_TOTAL_UNCOMPRESSED_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("declared uncompressed size exceeds {MAX_TOTAL_UNCOMPRESSED_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn read_limited<R: Read>(reader: &mut R, limit: u64, declared: u64) -> io::Result<Vec<u8>> {
    if declared > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("entry declares {declared} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = Vec::with_capacity(declared.min(limit) as usize);
    let mut buffer = [0u8; 8192];
    let mut total = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("entry exceeds {limit} byte read limit"),
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn parse_content_types(bytes: &[u8]) -> io::Result<ContentTypes> {
    let mut reader = Reader::from_reader(bytes);
    let mut result = ContentTypes::default();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(event)) | Ok(Event::Start(event)) => {
                match local_name(event.name().as_ref()) {
                    b"Default" => {
                        if let (Some(extension), Some(content_type)) = (
                            xml_attribute(&event, b"Extension"),
                            xml_attribute(&event, b"ContentType"),
                        ) {
                            result
                                .defaults
                                .insert(extension.to_ascii_lowercase(), content_type);
                        }
                    }
                    b"Override" => {
                        if let (Some(part), Some(content_type)) = (
                            xml_attribute(&event, b"PartName"),
                            xml_attribute(&event, b"ContentType"),
                        ) {
                            result.overrides.insert(
                                format!("/{}", normalize_package_path(&part)),
                                content_type,
                            );
                        }
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        }
        buffer.clear();
    }
    Ok(result)
}

fn parse_relationships(bytes: &[u8], source: &str) -> io::Result<Vec<Relationship>> {
    let mut reader = Reader::from_reader(bytes);
    let mut result = Vec::new();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(event)) | Ok(Event::Start(event))
                if local_name(event.name().as_ref()) == b"Relationship" =>
            {
                let id = required_xml_attribute(&event, b"Id", "Relationship")?;
                let relationship_type = required_xml_attribute(&event, b"Type", "Relationship")?;
                let target = required_xml_attribute(&event, b"Target", "Relationship")?;
                let external = xml_attribute(&event, b"TargetMode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                result.push(Relationship {
                    source: source.to_string(),
                    id,
                    relationship_type,
                    target: target.clone(),
                    resolved_target: (!external)
                        .then(|| resolve_relationship_target(source, &target))
                        .flatten(),
                    target_mode: if external {
                        TargetMode::External
                    } else {
                        TargetMode::Internal
                    },
                });
            }
            Ok(_) => {}
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        }
        buffer.clear();
    }
    Ok(result)
}

pub(crate) fn xml_attribute(event: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    // Attribute iterators are single-pass. Collecting first ensures a failed
    // exact-name lookup does not consume the candidates for namespace fallback.
    let attributes = event
        .attributes()
        .with_checks(true)
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    let requested_local = local_name(name);
    let attribute = attributes
        .iter()
        .find(|a| a.key.as_ref() == name)
        .or_else(|| {
            attributes.iter().find(|a| {
                local_name(a.key.as_ref()) == requested_local
                    && (!name.contains(&b':') || a.key.as_ref().contains(&b':'))
            })
        })?;
    attribute
        .decoded_and_normalized_value(quick_xml::XmlVersion::default(), event.decoder())
        .ok()
        .map(|v| v.into_owned())
}

fn required_xml_attribute(
    event: &BytesStart<'_>,
    name: &[u8],
    element: &str,
) -> io::Result<String> {
    xml_attribute(event, name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{element} is missing required {} attribute",
                    String::from_utf8_lossy(name)
                ),
            )
        })
}
fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}
fn relationship_source(path: &str) -> Option<String> {
    if path == "_rels/.rels" {
        return Some("/".into());
    }
    if let Some(name) = path.strip_prefix("_rels/") {
        return name
            .strip_suffix(".rels")
            .map(|s| format!("/{}", normalize_package_path(s)));
    }
    let (directory, name) = path.rsplit_once("/_rels/")?;
    Some(format!("/{directory}/{}", name.strip_suffix(".rels")?))
}
fn resolve_relationship_target(source: &str, target: &str) -> Option<String> {
    let combined = if target.starts_with('/') {
        target.to_string()
    } else {
        let directory = source.rsplit_once('/').map_or("", |(d, _)| d);
        format!("{directory}/{target}")
    };
    Some(format!("/{}", normalize_package_path(&combined)))
}

/// Canonical package identifiers always use `/`, have one leading slash at call sites,
/// and never retain `.` or `..` components.
pub fn normalize_package_path(path: &str) -> String {
    let mut components = Vec::new();
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            value => components.push(value),
        }
    }
    components.join("/")
}
/// Single source of truth for extension-based part classification. Used both for
/// `PartKind` assignment during indexing and for preview selection.
pub(crate) fn is_xml_name(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "xml" | "rels"))
}

pub(crate) fn is_image_name(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"
            )
        })
}

/// Maps an image file name to its decoder format. Keep in sync with `is_image_name`
/// and the enabled `image` crate features in `Cargo.toml`.
pub(crate) fn image_format(path: &str) -> ImageFormat {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        Some("gif") => ImageFormat::Gif,
        Some("bmp") => ImageFormat::Bmp,
        Some("webp") => ImageFormat::WebP,
        _ => ImageFormat::Png,
    }
}

impl ContentTypes {
    fn content_type_for(&self, path: &str) -> Option<String> {
        self.overrides.get(path).cloned().or_else(|| {
            path.rsplit('.')
                .next()
                .and_then(|e| self.defaults.get(&e.to_ascii_lowercase()).cloned())
        })
    }
}

pub(crate) fn is_xml_content_type(content_type: &str) -> bool {
    let value = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    value == "application/xml" || value == "text/xml" || value.ends_with("+xml")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_traversal_and_separators() {
        assert_eq!(normalize_package_path(r"/a\\b/../c"), "a/c");
    }

    #[test]
    fn file_type_detection_is_case_insensitive() {
        assert!(is_xml_name("custom.XML"));
        assert!(is_image_name("media/PHOTO.JpEg"));
        assert_eq!(image_format("media/PHOTO.JpEg"), ImageFormat::Jpeg);
    }

    #[test]
    fn xml_content_types_are_detected() {
        assert!(is_xml_content_type(
            "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"
        ));
        assert!(is_xml_content_type("application/xml"));
        assert!(!is_xml_content_type("application/json"));
        assert!(!is_xml_content_type("image/png"));
    }
    #[test]
    fn bounded_reader_rejects_declared_size() {
        let mut input = &b"abc"[..];
        assert!(read_limited(&mut input, 2, 3).is_err());
    }
    #[test]
    fn bounded_reader_enforces_actual_size() {
        let mut input = &b"abc"[..];
        assert!(read_limited(&mut input, 2, 0).is_err());
    }

    #[test]
    fn attributes_are_unescaped_and_namespace_aware() {
        let mut reader = Reader::from_str(r#"<x id="plain" r:id="a&amp;b"/>"#);
        let event = match reader.read_event() {
            Ok(Event::Empty(event)) => event,
            other => panic!("expected empty element, got {other:?}"),
        };
        assert_eq!(xml_attribute(&event, b"r:id").as_deref(), Some("a&b"));
        assert_eq!(xml_attribute(&event, b"id").as_deref(), Some("plain"));

        let mut reader = Reader::from_str(r#"<x r:id="fallback"/>"#);
        let event = match reader.read_event() {
            Ok(Event::Empty(event)) => event,
            other => panic!("expected empty element, got {other:?}"),
        };
        assert_eq!(xml_attribute(&event, b"id").as_deref(), Some("fallback"));
    }

    #[test]
    fn archive_limits_are_rejected_before_index_installation() {
        let count_error = check_archive_limits(MAX_ENTRY_COUNT + 1, 0).unwrap_err();
        assert!(count_error.to_string().contains("entries"));
        let size_error = check_archive_limits(1, MAX_TOTAL_UNCOMPRESSED_BYTES + 1).unwrap_err();
        assert!(size_error.to_string().contains("uncompressed size"));
    }

    #[cfg(test)]
    fn archive_with_declared_sizes(names: &[&str], declared_size: u32) -> Vec<u8> {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for name in names {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
        }
        let mut bytes = writer.finish().unwrap().into_inner();
        let local_header = b"PK\x03\x04";
        let central_header = b"PK\x01\x02";
        for offset in 0..bytes.len().saturating_sub(3) {
            if bytes[offset..].starts_with(local_header) {
                bytes[offset + 22..offset + 26].copy_from_slice(&declared_size.to_le_bytes());
            } else if bytes[offset..].starts_with(central_header) {
                bytes[offset + 24..offset + 28].copy_from_slice(&declared_size.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn ignored_entries_still_count_toward_total_size_limit() {
        let oversized_empty =
            archive_with_declared_sizes(&["/"], (MAX_TOTAL_UNCOMPRESSED_BYTES + 1) as u32);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(oversized_empty)).unwrap();
        let error = PackageIndex::from_archive(&mut archive).unwrap_err();
        assert!(error.to_string().contains("uncompressed size"));

        let half_limit = (MAX_TOTAL_UNCOMPRESSED_BYTES / 2 + 1) as u32;
        let colliding = archive_with_declared_sizes(&["part.xml", "./part.xml"], half_limit);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(colliding)).unwrap();
        let error = PackageIndex::from_archive(&mut archive).unwrap_err();
        assert!(error.to_string().contains("uncompressed size"));
    }

    #[test]
    fn over_deep_entries_are_skipped_before_indexing() {
        let deep_name = format!(
            "{}/file.xml",
            std::iter::repeat_n("directory", MAX_PATH_DEPTH)
                .collect::<Vec<_>>()
                .join("/")
        );
        let bytes = archive_with_declared_sizes(&[&deep_name], 1);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let index = PackageIndex::from_archive(&mut archive).unwrap();
        assert!(index.parts.is_empty());
        assert!(index
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("maximum depth")));
    }

    #[test]
    fn malformed_relationship_entry_is_rejected() {
        let error = parse_relationships(
            br#"<Relationships><Relationship Id="rId1" Type="type"/></Relationships>"#,
            "/doc.xml",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Target"));
    }
}
