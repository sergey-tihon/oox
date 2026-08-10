use std::{
    collections::HashMap,
    io::{self, Read},
};

use edtui::{EditorState, Lines};
use image::ImageFormat;
use quick_xml::{events::Event, Reader, Writer};
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

pub struct App {
    pub file_path: String,
    pub tree_state: TreeState<String>,
    pub tree_items: Vec<TreeItem<'static, String>>,
    pub editor_state: EditorState,
    pub image_state: Option<StatefulProtocol>,
    pub picker: Picker,
    pub current_widget: CurrentWidget,
    pub status_message: Option<String>,
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
    TextArea,
}

impl App {
    pub fn from_file(path: String, picker: Picker) -> io::Result<Self> {
        let file = std::fs::File::open(path.clone())?;
        let archive = zip::ZipArchive::new(file)?;

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

        Ok(Self {
            file_path: path,
            tree_state: TreeState::default(),
            tree_items,
            editor_state: EditorState::default(),
            image_state: None,
            picker,
            current_widget: CurrentWidget::Tree,
            status_message: Some("Select a package part to inspect".to_string()),
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

    pub fn load_selected_file_content(&mut self) -> io::Result<()> {
        // Reset the previous view before loading anything new. This prevents a
        // previously selected XML file or image from remaining visible when a
        // directory or unsupported package part is selected.
        self.image_state = None;
        self.editor_state = EditorState::default();
        self.status_message = Some("Select a package part to inspect".to_string());

        let selected = match self.tree_state.selected().last().cloned() {
            Some(selected) => selected,
            None => return Ok(()),
        };

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

        if Self::is_image(file_name) {
            match image::load_from_memory_with_format(&bytes, Self::image_format(file_name)) {
                Ok(image) => {
                    self.image_state = Some(self.picker.new_resize_protocol(image));
                    self.status_message = None;
                }
                Err(error) => {
                    self.status_message =
                        Some(format!("Could not decode image {display_name}: {error}"));
                }
            }
        } else if Self::is_xml(file_name) {
            let text = String::from_utf8_lossy(&bytes);
            match Self::pretty_print_xml(&text) {
                Ok(formatted) => {
                    self.editor_state = EditorState::new(Lines::from(formatted.as_str()));
                    self.status_message = None;
                }
                Err(error) => {
                    self.status_message =
                        Some(format!("Could not parse XML {display_name}: {error}"));
                }
            }
        } else {
            self.status_message = Some(format!(
                "Binary or unsupported file: {display_name} ({} bytes)",
                bytes.len()
            ));
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
        path.split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<&str>>()
            .join("/")
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
