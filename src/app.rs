use std::io::{self, Read};

use edtui::{EditorState, Lines};
use tui_tree_widget::{TreeItem, TreeState};
use xml::{EmitterConfig, EventReader};

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
    pub root: Node,
    pub tree_state: TreeState<String>,
    pub tree_items: Vec<TreeItem<'static, String>>,
    pub editor_state: EditorState,
    pub current_widget: CurrentWidget,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurrentWidget {
    Tree,
    TextArea,
}

impl App {
    pub fn from_file(path: String) -> io::Result<Self> {
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
            root,
            tree_state: TreeState::default(),
            tree_items,
            editor_state: EditorState::default(),
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
            let mut buf = String::new();
            entry.read_to_string(&mut buf)?;

            let formatted = Self::pretty_print_xml(&buf)?;
            self.editor_state = EditorState::new(Lines::from(formatted.as_str()));
        }

        Ok(())
    }

    fn pretty_print_xml(str: &str) -> io::Result<String> {
        let parser = EventReader::new(str.as_bytes());

        let mut buf = Vec::new();
        let mut writer = EmitterConfig::new()
            .perform_indent(true)
            .create_writer(&mut buf);

        for e in parser {
            match e {
                Ok(reader_event) => {
                    if let Some(writer_event) = reader_event.as_writer_event() {
                        writer.write(writer_event).unwrap();
                    }
                }
                Err(_) => todo!(),
            }
        }

        let res = String::from_utf8_lossy(&buf);
        Ok(res.to_string())
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
    use std::io;

    #[test]
    fn load_pptx() -> io::Result<()> {
        let app = App::from_file("data/sample.pptx".to_string())?;
        app.root.children.iter().for_each(|node| {
            println!("{}", node.name);
        });

        Ok(())
    }

    #[test]
    fn load_selected_file_content() -> io::Result<()> {
        let mut app = App::from_file("data/sample.pptx".to_string())?;
        app.tree_state.select_first();
        app.load_selected_file_content()?;
        app.load_selected_file_content()?;

        Ok(())
    }
}
