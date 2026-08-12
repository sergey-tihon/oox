use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    io::{self, Read},
};

use edtui::{EditorState, Lines};
use image::{DynamicImage, ImageFormat};
use quick_xml::{
    events::{BytesStart, Event},
    Reader, Writer,
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use tui_tree_widget::{TreeItem, TreeState};

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

#[derive(Clone, Debug, PartialEq)]
pub enum PartKind {
    Xml,
    Image,
    Binary,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Empty,
    Xml,
    PlainText,
    Json,
    Hex,
    Image,
    Info,
    Error,
}

#[derive(Debug)]
enum Preview {
    Editor { kind: PreviewKind, text: String },
    Image(DynamicImage),
    Info(String),
    Error(String),
}

const MAX_HEX_PREVIEW_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PartInfo {
    pub path: String,
    pub archive_name: String,
    pub content_type: Option<String>,
    pub size: u64,
    pub compressed_size: u64,
    pub kind: PartKind,
}

#[derive(Clone, Debug)]
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
pub struct ContentTypes {
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct DetailLink {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct DetailsView {
    pub text: String,
    pub links: Vec<DetailLink>,
}

#[derive(Clone, Debug, Default)]
pub struct PackageIndex {
    pub parts: BTreeMap<String, PartInfo>,
    pub relationships: Vec<Relationship>,
    pub outgoing: BTreeMap<String, Vec<usize>>,
    pub incoming: BTreeMap<String, Vec<usize>>,
    pub warnings: Vec<String>,
    content_types: ContentTypes,
}

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
    navigation_back: Vec<String>,
    navigation_forward: Vec<String>,
    navigation_current: Option<String>,
    pub show_help: bool,
    pub search_active: bool,
    pub search_query: String,
    search_matches: Vec<String>,
    search_index: Option<usize>,
    archive_paths: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurrentWidget {
    Tree,
    Details,
    TextArea,
}

impl PackageIndex {
    fn from_archive<R: Read + io::Seek>(archive: &mut zip::ZipArchive<R>) -> io::Result<Self> {
        let mut index = Self::default();
        let mut content_types_bytes = None;
        let mut relationship_parts = Vec::new();

        for entry_index in 0..archive.len() {
            let mut entry = archive.by_index(entry_index).map_err(io::Error::other)?;
            let archive_name = entry.name().to_string();
            let normalized_path = normalize_package_path(&archive_name);
            if normalized_path.is_empty() {
                continue;
            }

            let path = format!("/{normalized_path}");
            let is_directory = entry.is_dir();
            let kind = if is_directory {
                PartKind::Directory
            } else if App::is_xml(&archive_name) {
                PartKind::Xml
            } else if App::is_image(&archive_name) {
                PartKind::Image
            } else {
                PartKind::Binary
            };
            let size = entry.size();
            let compressed_size = entry.compressed_size();
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
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                content_types_bytes = Some(bytes);
            } else if archive_name.ends_with(".rels") {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                relationship_parts.push((normalized_path, bytes));
            }
        }

        if let Some(bytes) = content_types_bytes {
            match parse_content_types(&bytes) {
                Ok(content_types) => index.content_types = content_types,
                Err(error) => index
                    .warnings
                    .push(format!("Could not parse [Content_Types].xml: {error}")),
            }
        }

        for part in index.parts.values_mut() {
            part.content_type = index.content_types.content_type_for(&part.path);
        }

        for (relationship_path, bytes) in relationship_parts {
            let Some(source) = relationship_source(&relationship_path) else {
                index.warnings.push(format!(
                    "Could not determine relationship source: {relationship_path}"
                ));
                continue;
            };
            match parse_relationships(&bytes, &source) {
                Ok(mut relationships) => index.relationships.append(&mut relationships),
                Err(error) => index.warnings.push(format!(
                    "Could not parse relationship part /{relationship_path}: {error}"
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
}

impl ContentTypes {
    fn content_type_for(&self, path: &str) -> Option<String> {
        if let Some(content_type) = self.overrides.get(path) {
            return Some(content_type.clone());
        }
        let extension = path.rsplit('.').next()?.to_ascii_lowercase();
        self.defaults.get(&extension).cloned()
    }
}

fn parse_content_types(bytes: &[u8]) -> io::Result<ContentTypes> {
    let text = String::from_utf8_lossy(bytes);
    let mut reader = Reader::from_str(&text);
    let mut content_types = ContentTypes::default();
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(event)) | Ok(Event::Start(event)) => match event.name().as_ref() {
                b"Default" => {
                    if let (Some(extension), Some(content_type)) = (
                        xml_attribute(&event, b"Extension"),
                        xml_attribute(&event, b"ContentType"),
                    ) {
                        content_types
                            .defaults
                            .insert(extension.to_ascii_lowercase(), content_type);
                    }
                }
                b"Override" => {
                    if let (Some(part_name), Some(content_type)) = (
                        xml_attribute(&event, b"PartName"),
                        xml_attribute(&event, b"ContentType"),
                    ) {
                        content_types.overrides.insert(
                            format!("/{}", normalize_package_path(&part_name)),
                            content_type,
                        );
                    }
                }
                _ => {}
            },
            Ok(_) => {}
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        }
        buffer.clear();
    }

    Ok(content_types)
}

fn parse_relationships(bytes: &[u8], source: &str) -> io::Result<Vec<Relationship>> {
    let text = String::from_utf8_lossy(bytes);
    let mut reader = Reader::from_str(&text);
    let mut relationships = Vec::new();
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(event)) if event.name().as_ref() == b"Relationship" => {
                let id = xml_attribute(&event, b"Id").unwrap_or_default();
                let relationship_type = xml_attribute(&event, b"Type").unwrap_or_default();
                let target = xml_attribute(&event, b"Target").unwrap_or_default();
                let target_mode = if xml_attribute(&event, b"TargetMode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"))
                {
                    TargetMode::External
                } else {
                    TargetMode::Internal
                };
                let resolved_target = match target_mode {
                    TargetMode::External => None,
                    TargetMode::Internal => resolve_relationship_target(source, &target),
                };
                relationships.push(Relationship {
                    source: source.to_string(),
                    id,
                    relationship_type,
                    target,
                    resolved_target,
                    target_mode,
                });
            }
            Ok(_) => {}
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        }
        buffer.clear();
    }

    Ok(relationships)
}

fn xml_attribute(event: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    event
        .attributes()
        .with_checks(false)
        .filter_map(Result::ok)
        .find(|attribute| attribute.key.as_ref() == name)
        .map(|attribute| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
}

fn relationship_source(path: &str) -> Option<String> {
    if path == "_rels/.rels" {
        return Some("/".to_string());
    }
    if let Some(name) = path.strip_prefix("_rels/") {
        return name
            .strip_suffix(".rels")
            .map(|source| format!("/{}", normalize_package_path(source)));
    }
    let (directory, name) = path.rsplit_once("/_rels/")?;
    let source_name = name.strip_suffix(".rels")?;
    Some(format!("/{directory}/{source_name}"))
}

fn resolve_relationship_target(source: &str, target: &str) -> Option<String> {
    let combined = if target.starts_with('/') {
        target.to_string()
    } else {
        let directory = source
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        format!("{directory}/{target}")
    };
    Some(format!("/{}", normalize_package_path(&combined)))
}

fn normalize_package_path(path: &str) -> String {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => components.push(component),
        }
    }
    components.join("/")
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

fn build_preview(
    path: &str,
    content_type: Option<&str>,
    size: u64,
    compressed_size: u64,
    bytes: &[u8],
) -> Preview {
    if App::is_image(path) {
        return match image::load_from_memory_with_format(bytes, App::image_format(path)) {
            Ok(image) => Preview::Image(image),
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
            Err(error) => Preview::Error(format!("XML parsing failed: {error}")),
        };
    }

    if is_json_file(path, content_type) {
        let Some(text) = utf8_text(bytes) else {
            return Preview::Error("JSON is not valid UTF-8".to_string());
        };
        let formatted = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or(text),
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

impl App {
    pub fn from_file(path: String, picker: Picker) -> io::Result<Self> {
        let file = std::fs::File::open(path.clone())?;
        let mut archive = zip::ZipArchive::new(file)?;

        let mut root = Node::new("root", "");
        let mut archive_paths = HashMap::new();
        let file_names = archive
            .file_names()
            .map(str::to_owned)
            .collect::<Vec<String>>();

        for file_name in file_names {
            let normalized_path = App::normalize_path(&file_name);
            if normalized_path.is_empty() {
                continue;
            }

            let path = normalized_path.split('/').collect::<Vec<&str>>();
            App::build_tree(&mut root, &path, 0);
            archive_paths.insert(format!("/{normalized_path}"), file_name);
        }

        let tree_items = App::create_tree(&root)?;
        let package_index = PackageIndex::from_archive(&mut archive)?;

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
            navigation_back: Vec::new(),
            navigation_forward: Vec::new(),
            navigation_current: None,
            show_help: false,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_index: None,
            archive_paths,
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
        self.status_message = Some("Select a package part to inspect".to_string());

        let selected = match self.tree_state.selected().last().cloned() {
            Some(selected) => selected,
            None => return Ok(()),
        };
        if record_history {
            self.record_navigation(&selected);
        }

        let display_name = selected.trim_start_matches('/');
        let file_name = self
            .archive_paths
            .get(&selected)
            .map(String::as_str)
            .unwrap_or(display_name);

        let file = std::fs::File::open(self.file_path.clone())?;
        let mut zip = zip::ZipArchive::new(file)?;
        let is_directory = self.is_directory(&selected);
        let mut entry = match zip.by_name(file_name) {
            Ok(entry) => entry,
            Err(_) if is_directory => {
                self.status_message = Some(format!("Directory: {display_name}"));
                return Ok(());
            }
            Err(_) => {
                self.status_message = Some(format!("Unavailable package part: {display_name}"));
                return Ok(());
            }
        };

        if entry.is_dir() {
            self.status_message = Some(format!("Directory: {display_name}"));
            return Ok(());
        }

        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;

        let content_type = self
            .package_index
            .parts
            .get(&selected)
            .and_then(|part| part.content_type.as_deref());
        match build_preview(
            file_name,
            content_type,
            entry.size(),
            entry.compressed_size(),
            &bytes,
        ) {
            Preview::Editor { kind, text } => {
                self.preview_kind = kind;
                self.editor_state = EditorState::new(Lines::from(text.as_str()));
                self.status_message = None;
            }
            Preview::Image(image) => {
                self.preview_kind = PreviewKind::Image;
                self.image_state = Some(self.picker.new_resize_protocol(image));
                self.status_message = None;
            }
            Preview::Info(message) => {
                self.preview_kind = PreviewKind::Info;
                self.status_message = Some(message);
            }
            Preview::Error(message) => {
                self.preview_kind = PreviewKind::Error;
                self.status_message = Some(format!("Could not preview {display_name}: {message}"));
            }
        }

        Ok(())
    }

    fn is_directory(&self, path: &str) -> bool {
        self.archive_paths
            .get(path)
            .is_some_and(|archive_path| archive_path.ends_with('/'))
            || self
                .archive_paths
                .keys()
                .any(|child| child.starts_with(&format!("{path}/")))
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
            .archive_paths
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

    fn normalize_path(path: &str) -> String {
        normalize_package_path(path)
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
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);

        let mut output = Vec::new();
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

        String::from_utf8(output).map_err(io::Error::other)
    }

    fn build_tree(node: &mut Node, parts: &[&str], depth: usize) {
        if depth < parts.len() {
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
    use crate::App;
    use ratatui_image::picker::Picker;
    use std::io;

    #[test]
    fn load_pptx() -> io::Result<()> {
        let app = App::from_file("data/sample.pptx".to_string(), Picker::halfblocks())?;
        assert!(!app.tree_items.is_empty());

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
            App::normalize_path("/ppt//slides/./slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(App::normalize_path("///"), "");
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
