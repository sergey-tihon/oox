use std::io::{self, Read};

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

    fn find_child(&mut self, name: &str) -> Option<&mut Self> {
        self.children.iter_mut().find(|c| c.name == name)
    }

    fn add_child<T>(&mut self, leaf: T) -> &mut Self
    where
        T: Into<Self>,
    {
        self.children.push(leaf.into());
        self
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

        for file in archive.file_names() {
            let path = file.split('/').collect::<Vec<&str>>();
            App::build_tree(&mut root, &path, 0);
        }

        let tree_items = App::create_tree(&root);

        Ok(Self {
            file_path: path,
            tree_state: TreeState::default(),
            tree_items,
            editor_state: EditorState::default(),
            image_state: None,
            picker,
            current_widget: CurrentWidget::Tree,
        })
    }

    pub fn load_selected_file_content(&mut self) -> io::Result<()> {
        let selected = self.tree_state.selected();
        let file_name = match selected.last() {
            Some(x) => x,
            None => return Ok(()),
        };

        let file = std::fs::File::open(self.file_path.clone())?;
        let mut zip = zip::ZipArchive::new(file)?;

        let file_name = file_name.trim_start_matches('/');
        if let Ok(mut entry) = zip.by_name(file_name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;

            if Self::is_image(file_name) {
                let image =
                    image::load_from_memory_with_format(&bytes, Self::image_format(file_name))
                        .map_err(io::Error::other)?;
                self.image_state = Some(self.picker.new_resize_protocol(image));
            } else if Self::is_xml(file_name) {
                let text = String::from_utf8_lossy(&bytes);
                let formatted = Self::pretty_print_xml(&text)?;
                self.editor_state = EditorState::new(Lines::from(formatted.as_str()));
                self.image_state = None;
            }
        }

        Ok(())
    }

    fn is_xml(path: &str) -> bool {
        path.ends_with(".xml") || path.ends_with(".rels")
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

    fn build_tree(node: &mut Node, parts: &Vec<&str>, depth: usize) {
        if depth < parts.len() {
            let item = &parts[depth];

            let dir = match node.find_child(item) {
                Some(d) => d,
                None => {
                    let path = node.path.to_owned() + "/" + item;
                    let d = Node::new(item, &path);
                    node.add_child(d);
                    match node.find_child(item) {
                        Some(d2) => d2,
                        None => panic!("Got here!"),
                    }
                }
            };
            App::build_tree(dir, parts, depth + 1);
        }
    }

    fn create_tree(root: &Node) -> Vec<TreeItem<'static, String>> {
        fn to_tree_item(node: &Node) -> TreeItem<'static, String> {
            let text = node.name.to_owned();
            let identifier = node.path.to_owned();

            if node.children.is_empty() {
                TreeItem::new_leaf(identifier, text)
            } else {
                TreeItem::new(identifier, text, parse_children(node)).unwrap()
            }
        }
        fn parse_children(node: &Node) -> Vec<TreeItem<'static, String>> {
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
        app.tree_state.select_first();
        app.load_selected_file_content()?;
        app.load_selected_file_content()?;

        Ok(())
    }
}
