use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use edtui::{EditorState, Lines};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use tui_tree_widget::{TreeItem, TreeState};

use crate::package::{
    is_image_name, is_xml_name, Package, PackageIndex, PartKind, Relationship, TargetMode,
};
use crate::preview::{Preview, PreviewKind};
use crate::summary::{DetailLink, DetailsView};
use crate::worker::{accepts_result, Job, ResultMessage, Worker};

/// Bounds the back/forward navigation history so long sessions cannot grow it
/// without limit.
const MAX_NAVIGATION_HISTORY: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurrentWidget {
    Tree,
    Details,
    TextArea,
}

pub struct App {
    pub file_path: String,
    pub tree_state: TreeState<String>,
    pub tree_items: Vec<TreeItem<'static, String>>,
    pub editor_state: EditorState,
    pub image_state: Option<StatefulProtocol>,
    pub preview_kind: PreviewKind,
    picker: Picker,
    pub current_widget: CurrentWidget,
    /// Message rendered in the content pane when no editor/image/summary is shown.
    /// The bottom status bar is driven by `selection_status`, not by this field.
    pub content_message: Option<String>,
    pub details_visible: bool,
    pub details_scroll: u16,
    pub details_cursor: usize,
    details_cache: DetailsView,
    details_cache_key: Option<(u64, Option<String>)>,
    details_generation: u64,
    pub document_summary: Option<DetailsView>,
    package: Option<Package>,
    worker: Worker,
    open_request_id: u64,
    preview_request_id: u64,
    pub loading: bool,
    preview_pending: bool,
    pub worker_error: Option<String>,
    pub summary_visible: bool,
    pub summary_scroll: u16,
    navigation_back: VecDeque<String>,
    navigation_forward: Vec<String>,
    navigation_current: Option<String>,
    pub show_help: bool,
    pub search_active: bool,
    pub search_query: String,
    search_matches: Vec<String>,
    search_index: Option<usize>,
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
            content_message: Some("Loading package…".to_string()),
            details_visible: true,
            details_scroll: 0,
            details_cursor: 0,
            details_cache: DetailsView::default(),
            details_cache_key: None,
            details_generation: 0,
            document_summary: None,
            package: None,
            worker,
            open_request_id: 1,
            preview_request_id: 0,
            loading: true,
            preview_pending: false,
            worker_error: None,
            summary_visible: false,
            summary_scroll: 0,
            navigation_back: VecDeque::new(),
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

    /// The package index of the loaded package, or a shared empty index while loading.
    pub fn index(&self) -> &PackageIndex {
        self.package
            .as_ref()
            .map(|package| &*package.index)
            .unwrap_or_else(|| {
                static EMPTY: OnceLock<PackageIndex> = OnceLock::new();
                EMPTY.get_or_init(PackageIndex::default)
            })
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
                    self.preview_pending = false;
                    self.worker_error = Some(error.to_string());
                    self.content_message = Some(format!("Package worker failed: {error}"));
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
                            self.package = Some(package);
                            self.details_generation = self.details_generation.wrapping_add(1);
                            self.editor_state = EditorState::default();
                            self.image_state = None;
                            self.preview_kind = PreviewKind::Empty;
                            self.install_tree();
                            self.document_summary = summary.view;
                            self.loading = false;
                            self.content_message = Some(
                                "Select a package part or press Enter to preview content"
                                    .to_string(),
                            );
                        }
                        Err(error) => {
                            self.loading = false;
                            self.worker_error = Some(error.clone());
                            self.content_message = Some(format!("Could not open package: {error}"));
                        }
                    }
                }
                ResultMessage::PartRead {
                    request_id,
                    selected_path,
                    preview,
                } => {
                    if request_id == self.preview_request_id {
                        self.preview_pending = false;
                    }
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
                            self.content_message = None;
                        }
                        Ok(Preview::Image(image)) => {
                            self.preview_kind = PreviewKind::Image;
                            self.image_state = Some(self.picker.new_resize_protocol(image));
                            self.content_message = None;
                        }
                        Ok(Preview::Info(message)) => {
                            self.preview_kind = PreviewKind::Info;
                            self.content_message = Some(message);
                        }
                        Ok(Preview::Error(message)) => {
                            self.preview_kind = PreviewKind::Error;
                            self.content_message = Some(format!(
                                "Could not preview {}: {message}",
                                selected_path.trim_start_matches('/')
                            ));
                        }
                        Err(error) => {
                            self.preview_kind = PreviewKind::Error;
                            self.content_message = Some(format!("Could not preview: {error}"));
                        }
                    }
                }
            }
        }
        // Watchdog: explicit in-flight flags instead of inspecting message text.
        if !self.worker.is_alive() && (self.loading || self.preview_pending) {
            self.loading = false;
            self.preview_pending = false;
            let message = "Package worker exited before completing the request".to_string();
            self.worker_error = Some(message.clone());
            self.content_message = Some(message);
            changed = true;
        }
        changed
    }

    pub fn is_package_loaded(&self) -> bool {
        !self.loading && self.package.is_some()
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
            self.content_message = Some("No document-specific summary is available".to_string());
            return Ok(());
        }

        self.summary_visible = true;
        self.summary_scroll = 0;
        self.image_state = None;
        self.editor_state = EditorState::default();
        self.preview_kind = PreviewKind::Summary;
        self.content_message = None;
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

    /// The metadata view for the current selection. The view is cached and only
    /// rebuilt when the selection or the loaded package changes, so callers may
    /// invoke it freely (per frame, per cursor move) without allocation churn.
    pub fn details_view(&mut self) -> &DetailsView {
        let key = (
            self.details_generation,
            self.tree_state.selected().last().cloned(),
        );
        if self.details_cache_key.as_ref() != Some(&key) {
            self.details_cache = self.build_details_view();
            self.details_cache_key = Some(key);
        }
        &self.details_cache
    }

    fn build_details_view(&self) -> DetailsView {
        let Some(selected) = self.tree_state.selected().last() else {
            return DetailsView {
                text: "Select a package part to see metadata\n".to_string(),
                links: Vec::new(),
            };
        };

        let index = self.index();
        let mut text = String::new();
        let mut links = Vec::new();
        let display_name = selected.trim_start_matches('/');
        push_detail_line(&mut text, &format!("Part: {display_name}"));

        if let Some(part) = index.parts.get(selected) {
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
        let outgoing = index.outgoing.get(selected);
        let incoming = index.incoming.get(selected);
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
                let relationship = &index.relationships[*relationship_index];
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
                let relationship = &index.relationships[*relationship_index];
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

        if !index.warnings.is_empty() {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Warnings");
            for warning in &index.warnings {
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
        let links_len = self.details_view().links.len();
        if links_len == 0 {
            self.scroll_details(if reverse { -1 } else { 1 });
            return;
        }

        self.details_cursor = if reverse {
            self.details_cursor.checked_sub(1).unwrap_or(links_len - 1)
        } else {
            (self.details_cursor + 1) % links_len
        };
        let cursor = self.details_cursor;
        let line = self.details_view().links[cursor].line;
        self.details_scroll = line.saturating_sub(2) as u16;
    }

    pub fn activate_current_detail_link(&mut self) -> io::Result<bool> {
        let cursor = self.details_cursor;
        let Some((line, start)) = self
            .details_view()
            .links
            .get(cursor)
            .map(|link| (link.line, link.start))
        else {
            return Ok(false);
        };
        self.activate_detail_link(line, start)
    }

    pub fn activate_detail_link(&mut self, line: usize, column: usize) -> io::Result<bool> {
        let target = {
            let view = self.details_view();
            let Some(link) = view
                .links
                .iter()
                .find(|link| link.line == line && column >= link.start && column < link.end)
            else {
                return Ok(false);
            };
            link.target.clone()
        };

        if !self.index().parts.contains_key(&target) && !self.is_directory(&target) {
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

        if !self.index().parts.contains_key(&target) && !self.is_directory(&target) {
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
            .content_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.content_message = None;
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
            .content_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.content_message = None;
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
        let path = self.search_matches[next].clone();
        self.select_path(&path);
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
        } else if is_xml_name(display_name) {
            "XML"
        } else if is_image_name(display_name) {
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
            self.navigation_back.push_back(current);
            while self.navigation_back.len() > MAX_NAVIGATION_HISTORY {
                self.navigation_back.pop_front();
            }
        }
        self.navigation_forward.clear();
    }

    pub fn navigate_back(&mut self) -> io::Result<bool> {
        let Some(previous) = self.navigation_back.pop_back() else {
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
            self.navigation_back.push_back(current);
            while self.navigation_back.len() > MAX_NAVIGATION_HISTORY {
                self.navigation_back.pop_front();
            }
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
        self.content_message = Some("Select a package part to inspect".to_string());

        let selected = match self.tree_state.selected().last().cloned() {
            Some(selected) => selected,
            None => return Ok(()),
        };
        if record_history {
            self.record_navigation(&selected);
        }

        let display_name = selected.trim_start_matches('/').to_string();
        let Some(part) = self.index().parts.get(&selected).cloned() else {
            if self.is_directory(&selected) {
                self.content_message = Some(format!("Directory: {display_name}"));
            } else {
                self.content_message = Some(format!("Unavailable package part: {display_name}"));
            }
            return Ok(());
        };
        if part.kind == PartKind::Directory {
            self.content_message = Some(format!("Directory: {display_name}"));
            return Ok(());
        }
        let Some((package_source, index)) = self
            .package
            .as_ref()
            .map(|package| (package.source.clone(), Arc::clone(&package.index)))
        else {
            self.content_message = Some("Package is still loading".to_string());
            return Ok(());
        };
        self.preview_request_id = self.preview_request_id.wrapping_add(1);
        if let Err(error) = self.worker.submit(Job::ReadPart {
            request_id: self.preview_request_id,
            package_path: package_source,
            part: Box::new(part),
            index,
        }) {
            self.preview_pending = false;
            self.preview_kind = PreviewKind::Error;
            self.worker_error = Some(error.to_string());
            self.content_message = Some(format!("Package worker failed: {error}"));
            return Ok(());
        }
        self.preview_pending = true;
        self.content_message = Some(format!("Loading {display_name}…"));
        Ok(())
    }

    fn is_directory(&self, path: &str) -> bool {
        self.index().is_directory(path)
    }

    fn install_tree(&mut self) {
        // BTreeMap keys iterate in sorted order, which the tree builder relies on.
        let paths: Vec<String> = self
            .index()
            .parts
            .keys()
            .map(|path| path.trim_start_matches('/'))
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect();
        match create_tree(&paths) {
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

        let mut matches: Vec<String> = self
            .index()
            .parts
            .keys()
            .filter(|path| path.to_ascii_lowercase().contains(&query))
            .cloned()
            .collect();
        matches.sort();
        self.search_matches = matches;

        if self.search_matches.is_empty() {
            self.search_index = None;
            self.content_message = Some(format!("No package parts match: {}", self.search_query));
            return;
        }

        self.search_index = Some(0);
        let path = self.search_matches[0].clone();
        self.select_path(&path);
    }
}

/// Build tree items directly from sorted, normalized package paths (no leading
/// slash) without an intermediate node structure.
fn create_tree(paths: &[String]) -> io::Result<Vec<TreeItem<'static, String>>> {
    create_tree_level("", paths, 0)
}

/// `offset` is the byte length of the shared ancestor prefix including its
/// trailing slash, so recursion never re-allocates path components. Paths are
/// sorted, which groups a directory's children contiguously after it.
fn create_tree_level(
    parent: &str,
    paths: &[String],
    offset: usize,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut items = Vec::new();
    let mut index = 0;
    while index < paths.len() {
        let rest = &paths[index][offset..];
        let head = rest.split('/').next().unwrap_or(rest);
        let identifier = format!("{parent}/{head}");
        // A directory entry itself ("head") sorts before its children
        // ("head/..."); consume it so leaf and branch merge into one node.
        if rest.len() == head.len() {
            index += 1;
        }
        let prefix = format!("{head}/");
        let children_start = index;
        while index < paths.len() && paths[index][offset..].starts_with(&prefix) {
            index += 1;
        }
        let children = &paths[children_start..index];
        if children.is_empty() {
            items.push(TreeItem::new_leaf(identifier, head.to_string()));
        } else {
            let child_offset = offset + head.len() + 1;
            let children = create_tree_level(&identifier, children, child_offset)?;
            items.push(
                TreeItem::new(identifier, head.to_string(), children).map_err(io::Error::other)?,
            );
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use crate::preview::PreviewKind;
    use crate::{worker::Worker, App};
    use ratatui_image::picker::Picker;
    use std::{io, time::Duration};

    /// Pump the worker until `done` holds, with a generous timeout. Tests run the
    /// real worker thread; they only avoid fixed sleeps.
    fn pump_until(app: &mut App, done: impl Fn(&App) -> bool) {
        for _ in 0..1_000 {
            if done(app) {
                return;
            }
            app.poll_worker();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(done(app), "timed out waiting for background work");
    }

    fn test_app(path: &str) -> io::Result<App> {
        let worker = Worker::start()?;
        let mut app = App::new_loading(path.to_string(), Picker::halfblocks(), worker)?;
        pump_until(&mut app, |app| !app.loading);
        Ok(app)
    }

    fn preview_loaded(app: &mut App) {
        pump_until(app, |app| !app.preview_pending);
    }

    #[test]
    fn loading_constructor_installs_worker_package_result() -> io::Result<()> {
        let worker = Worker::start()?;
        let mut app =
            App::new_loading("data/sample.pptx".to_string(), Picker::halfblocks(), worker)?;
        assert!(app.loading);
        assert!(app.tree_items.is_empty());
        pump_until(&mut app, |app| !app.loading);
        assert!(app.is_package_loaded());
        assert!(!app.tree_items.is_empty());
        assert!(app.document_summary.is_some());
        Ok(())
    }

    #[test]
    fn loading_error_keeps_no_package_state() -> io::Result<()> {
        let mut app = test_app("/definitely/not/a/package.pptx")?;
        assert!(!app.is_package_loaded());
        assert!(app.tree_items.is_empty());
        assert!(app.selection_status().contains("No package"));
        app.expand_all();
        app.collapse_all();
        Ok(())
    }

    #[test]
    fn load_pptx() -> io::Result<()> {
        let app = test_app("data/sample.pptx")?;
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
        let mut app = test_app("data/sample.pptx")?;
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
        preview_loaded(&mut app);
        assert_eq!(app.preview_kind, PreviewKind::Xml);
        Ok(())
    }

    #[test]
    fn toggle_document_summary_switches_back_to_selected_content() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.toggle_summary()?;
        assert_eq!(app.preview_kind, PreviewKind::Summary);
        assert!(app.summary_visible);

        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.toggle_summary()?;
        assert!(!app.summary_visible);
        preview_loaded(&mut app);
        assert_eq!(app.preview_kind, PreviewKind::Xml);
        assert!(app.content_message.is_none());
        Ok(())
    }

    #[test]
    fn load_selected_file_content() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        assert!(app.details_visible);
        app.tree_state
            .select(vec!["/ppt/media/image1.gif".to_string()]);
        app.load_selected_file_content()?;
        preview_loaded(&mut app);
        assert!(app.image_state.is_some());
        assert!(app.content_message.is_none());

        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.load_selected_file_content()?;
        preview_loaded(&mut app);
        assert!(app.image_state.is_none());
        assert!(app.content_message.is_none());

        app.tree_state.select(vec!["/ppt".to_string()]);
        app.load_selected_file_content()?;
        assert!(app.image_state.is_none());
        assert_eq!(app.content_message.as_deref(), Some("Directory: ppt"));

        Ok(())
    }

    #[test]
    fn expand_and_collapse_all_tree_nodes() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.expand_all();
        assert!(!app.tree_state.opened().is_empty());

        app.collapse_all();
        assert!(app.tree_state.opened().is_empty());
        assert_eq!(app.tree_state.selected().len(), 1);

        Ok(())
    }

    #[test]
    fn navigation_history_moves_back_and_forward() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.load_selected_file_content()?;
        preview_loaded(&mut app);
        app.tree_state.select(vec![
            "/ppt".to_string(),
            "/ppt/presentation.xml".to_string(),
        ]);
        app.load_selected_file_content()?;
        preview_loaded(&mut app);

        assert!(app.navigate_back()?);
        preview_loaded(&mut app);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/[Content_Types].xml")
        );
        assert!(app.navigate_forward()?);
        preview_loaded(&mut app);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/presentation.xml")
        );

        Ok(())
    }

    #[test]
    fn navigation_history_is_capped() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        for index in 0..(super::MAX_NAVIGATION_HISTORY + 50) {
            app.record_navigation(&format!("/part-{index}.xml"));
        }
        assert!(app.navigation_back.len() <= super::MAX_NAVIGATION_HISTORY);
        Ok(())
    }

    #[test]
    fn details_view_is_cached_per_selection() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        let first = app.details_view().text.clone();
        let second = app.details_view().text.clone();
        assert_eq!(first, second);
        assert!(first.contains("[Content_Types].xml"));

        app.tree_state.select(vec!["/ppt".to_string()]);
        let third = app.details_view().text.clone();
        assert!(third.contains("ppt"));
        assert_ne!(first, third);
        Ok(())
    }

    #[test]
    fn package_metadata_and_relationships_are_indexed() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        let slide = app
            .index()
            .parts
            .get("/ppt/slides/slide1.xml")
            .expect("sample slide should be indexed");
        assert_eq!(slide.kind, crate::package::PartKind::Xml);
        assert!(slide
            .content_type
            .as_deref()
            .is_some_and(|content_type| { content_type.contains("presentationml.slide+xml") }));

        let outgoing = app
            .index()
            .outgoing
            .get("/ppt/slides/slide1.xml")
            .expect("sample slide should have relationships");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(
            app.index().relationships[outgoing[0]]
                .resolved_target
                .as_deref(),
            Some("/ppt/slideLayouts/slideLayout1.xml")
        );

        app.tree_state.select(vec![
            "/ppt".to_string(),
            "/ppt/slides".to_string(),
            "/ppt/slides/slide1.xml".to_string(),
        ]);
        let (line, start) = {
            let view = app.details_view();
            let link = view.links.first().expect("details should have links");
            (link.line, link.start)
        };
        app.activate_detail_link(line, start)?;
        preview_loaded(&mut app);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/slideLayouts/slideLayout1.xml")
        );

        Ok(())
    }

    #[test]
    fn search_selects_matching_package_parts() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
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

    #[test]
    fn tree_builder_merges_directory_entries_with_their_children() -> io::Result<()> {
        let paths: Vec<String> = [
            "[Content_Types].xml",
            "ppt",
            "ppt/slides",
            "ppt/slides/a.xml",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect();
        let items = super::create_tree(&paths)?;
        assert_eq!(items.len(), 2);
        let ppt = &items[1];
        assert_eq!(ppt.identifier(), "/ppt");
        assert_eq!(ppt.children().len(), 1);
        assert_eq!(ppt.children()[0].identifier(), "/ppt/slides");
        Ok(())
    }
}
