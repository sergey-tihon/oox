use std::{
    collections::{HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use edtui::{EditorState, Lines, RowIndex};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use tui_tree_widget::{TreeItem, TreeState};

use crate::package::{
    DiagnosticSeverity, Package, PackageIndex, PartInfo, PartKind, Relationship, TargetMode,
    is_image_name, is_xml_name,
};
use crate::preview::{Preview, PreviewKind};
use crate::summary::{DetailLink, DetailsView};
use crate::worker::{
    ExportMode, ExportOutcome, Job, ResultMessage, TempPart, Worker, accepts_result,
};

/// Bounds the back/forward navigation history so long sessions cannot grow it
/// without limit.
const MAX_NAVIGATION_HISTORY: usize = 256;
const MAX_CONTENT_SEARCH_QUERY_CHARS: usize = 256;
const MAX_EXPORT_PATH_CHARS: usize = 1024;
/// Integrity issues rendered in the metadata panel; the rest are summarized.
const MAX_INTEGRITY_LINES: usize = 50;

/// Work an export produced that only the event loop can finish: running a
/// command needs the terminal, and OSC 52 needs the backend's writer.
pub enum PendingExport {
    Extracted(PathBuf),
    OpenTemp(TempPart),
    Clipboard(String),
}

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
    /// Live path filter during search; `None` shows the full tree.
    filtered_tree_items: Option<Vec<TreeItem<'static, String>>>,
    /// Open/closed tree state from before the filter, restored when it clears.
    opened_before_search: Option<Vec<Vec<String>>>,
    pub editor_state: EditorState,
    pub image_state: Option<StatefulProtocol>,
    pub preview_kind: PreviewKind,
    /// The part whose content the preview currently shows. The tree selection
    /// can move on without reloading the preview, so relationship references in
    /// the editor must be resolved against this path, not the selection.
    previewed_path: Option<String>,
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
    pub content_search_active: bool,
    pub content_search_query: String,
    content_search_matches: Vec<String>,
    content_search_index: Option<usize>,
    content_search_request_id: u64,
    content_search_pending: bool,
    pub export_active: bool,
    pub export_query: String,
    export_request_id: u64,
    export_pending: bool,
    pending_export: Option<PendingExport>,
    /// One-line feedback for work that has no other visible surface (export
    /// results). Rendered in the status bar and cleared on the next selection.
    pub status_message: Option<String>,
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
            filtered_tree_items: None,
            opened_before_search: None,
            editor_state: EditorState::default(),
            image_state: None,
            preview_kind: PreviewKind::Empty,
            previewed_path: None,
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
            content_search_active: false,
            content_search_query: String::new(),
            content_search_matches: Vec::new(),
            content_search_index: None,
            content_search_request_id: 0,
            content_search_pending: false,
            export_active: false,
            export_query: String::new(),
            export_request_id: 0,
            export_pending: false,
            pending_export: None,
            status_message: None,
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
                    self.content_search_pending = false;
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
                            self.previewed_path = None;
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
                            self.previewed_path = Some(selected_path);
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
                ResultMessage::ContentSearch {
                    request_id,
                    query,
                    matches,
                } => {
                    if request_id != self.content_search_request_id {
                        continue;
                    }
                    self.content_search_pending = false;
                    match matches {
                        Ok(matches) => {
                            self.content_search_matches = matches;
                            self.content_search_index =
                                (!self.content_search_matches.is_empty()).then_some(0);
                            let paths = self.content_search_matches.clone();
                            self.apply_content_search_filter(&paths);
                            if let Some(path) = self.content_search_matches.first().cloned() {
                                self.select_path(&path);
                                self.content_message = None;
                            } else {
                                self.content_message =
                                    Some(format!("No package contents match: {query}"));
                            }
                        }
                        Err(error) => {
                            self.content_search_matches.clear();
                            self.content_search_index = None;
                            self.filtered_tree_items = Some(Vec::new());
                            self.content_message = Some(format!("Content search failed: {error}"));
                        }
                    }
                }
                ResultMessage::Exported {
                    request_id,
                    outcome,
                } => {
                    if request_id != self.export_request_id {
                        // A stale `TempPart` removes its file as it drops.
                        continue;
                    }
                    self.export_pending = false;
                    match outcome {
                        Ok(ExportOutcome::Saved(path)) => {
                            self.pending_export = Some(PendingExport::Extracted(path));
                        }
                        Ok(ExportOutcome::TempFile(temp)) => {
                            self.pending_export = Some(PendingExport::OpenTemp(temp));
                        }
                        Ok(ExportOutcome::Clipboard(text)) => {
                            self.pending_export = Some(PendingExport::Clipboard(text));
                        }
                        Err(error) => {
                            self.status_message = Some(format!("Export failed: {error}"));
                        }
                    }
                }
            }
        }
        // Watchdog: explicit in-flight flags instead of inspecting message text.
        if !self.worker.is_alive()
            && (self.loading
                || self.preview_pending
                || self.content_search_pending
                || self.export_pending)
        {
            self.loading = false;
            self.preview_pending = false;
            self.content_search_pending = false;
            self.export_pending = false;
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
        let paths = collect_open_paths(self.visible_tree_items());
        for path in paths {
            self.tree_state.open(path);
        }
    }

    pub fn collapse_all(&mut self) {
        self.tree_state.close_all();
        if let Some(first) = self.visible_tree_items().first() {
            let identifier = first.identifier().clone();
            self.tree_state.select(vec![identifier]);
        } else {
            self.tree_state.select(Vec::new());
        }
    }

    /// Tree items currently rendered: the live filter result while searching,
    /// otherwise the full package tree.
    pub fn visible_tree_items(&self) -> &[TreeItem<'static, String>] {
        self.filtered_tree_items
            .as_deref()
            .unwrap_or(&self.tree_items)
    }

    /// Whether a search filter currently hides parts of the tree.
    pub fn tree_filter_active(&self) -> bool {
        self.filtered_tree_items.is_some()
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

        let issues = &index.integrity;
        if !issues.is_empty() {
            push_detail_line(&mut text, "");
            push_detail_line(
                &mut text,
                &format!("Integrity issues ({})  [i/I jump]", issues.len()),
            );
            for issue in issues.iter().take(MAX_INTEGRITY_LINES) {
                let severity = match issue.severity {
                    DiagnosticSeverity::Error => "error",
                    DiagnosticSeverity::Warning => "warn",
                };
                let prefix = format!("  [{severity}] ");
                let message = compact_text(&issue.message, 64);
                let line_number = push_detail_line(&mut text, &format!("{prefix}{message}"));
                if let Some(part) = issue.part.as_deref() {
                    let start = prefix.chars().count();
                    links.push(DetailLink {
                        line: line_number,
                        start,
                        end: start + message.chars().count(),
                        target: part.to_string(),
                    });
                }
            }
            if issues.len() > MAX_INTEGRITY_LINES {
                push_detail_line(
                    &mut text,
                    &format!("  … {} more", issues.len() - MAX_INTEGRITY_LINES),
                );
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
        // An applied filter could hide the destination, so a link that selected
        // an invisible item would look like it did nothing.
        self.cancel_any_search();
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

    /// Jump to the relationship target referenced by the token under the editor
    /// cursor, recording the jump in the navigation history. Returns `false`
    /// when the cursor is not on a relationship reference, so callers can fall
    /// back to normal key handling.
    pub fn follow_relationship_at_cursor(&mut self) -> io::Result<bool> {
        let Some(relationship) = self.relationship_at_cursor() else {
            return Ok(false);
        };
        if relationship.target_mode == TargetMode::External {
            self.status_message = Some(format!("External target: {}", relationship.target));
            return Ok(true);
        }
        let Some(target) = relationship.resolved_target.clone() else {
            self.status_message = Some(format!("Relationship {} has no target", relationship.id));
            return Ok(true);
        };
        if !self.index().parts.contains_key(&target) && !self.is_directory(&target) {
            self.status_message = Some(format!("Relationship target is missing: {target}"));
            return Ok(true);
        }
        // An applied filter could hide the destination, which would make the
        // jump look like it did nothing.
        self.cancel_any_search();
        self.select_path(&target);
        self.load_selected_file_content()?;
        Ok(true)
    }

    /// The relationship referenced by the token under the editor cursor. Only an
    /// XML preview can reference relationships, and only `r:*` attribute values
    /// are treated as references.
    fn relationship_at_cursor(&self) -> Option<Relationship> {
        if self.preview_kind != PreviewKind::Xml {
            return None;
        }
        let previewed = self.previewed_path.as_deref()?;
        let index = self.index();
        let relationships = index.outgoing.get(previewed)?;
        let chars = self
            .editor_state
            .lines
            .get(RowIndex::new(self.editor_state.cursor.row))?;
        let column = self.editor_state.cursor.col;

        for relationship_index in relationships {
            let relationship = &index.relationships[*relationship_index];
            if relationship.id.is_empty() {
                continue;
            }
            let needle: Vec<char> = format!("\"{}\"", relationship.id).chars().collect();
            let width = needle.len();
            for value_quote in 0..chars.len().saturating_sub(width - 1) {
                // `="value"` rules out the same text in element content.
                if value_quote == 0 || chars[value_quote - 1] != '=' {
                    continue;
                }
                if chars[value_quote..value_quote + width] != needle[..] {
                    continue;
                }
                // The token spans the attribute name too, so the cursor on
                // either side of `=` follows the reference.
                let mut token_start = value_quote - 1;
                while token_start > 0
                    && matches!(
                        chars[token_start - 1],
                        'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | ':' | '.' | '-'
                    )
                {
                    token_start -= 1;
                }
                if chars[token_start] != 'r' || chars.get(token_start + 1) != Some(&':') {
                    continue;
                }
                if column >= token_start && column < value_quote + width {
                    return Some(relationship.clone());
                }
            }
        }
        None
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
        self.cancel_any_search();
        self.summary_visible = false;
        self.summary_scroll = 0;
        self.select_path(&target);
        self.load_selected_file_content_inner(true)?;
        Ok(true)
    }

    pub fn start_search(&mut self) {
        if self.content_search_active || !self.content_search_query.is_empty() {
            self.cancel_content_search();
        }
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
        self.filtered_tree_items = None;
        // Keep the earliest snapshot so re-entering search while a filter is
        // applied still restores the original open/closed state on cancel.
        if self.opened_before_search.is_none() {
            self.opened_before_search = Some(self.tree_state.opened().iter().cloned().collect());
        }
    }

    pub fn search_input_char(&mut self, character: char) {
        self.search_query.push(character);
        self.update_tree_filter();
    }

    pub fn search_backspace(&mut self) {
        self.search_query.pop();
        self.update_tree_filter();
    }

    pub fn finish_search(&mut self) {
        self.search_active = false;
        self.update_search_matches();
        if self.search_query.is_empty() {
            self.opened_before_search = None;
        }
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
        self.clear_tree_filter();
    }

    /// Drop the filtered view and restore the open/closed state the tree had
    /// before the search started.
    fn clear_tree_filter(&mut self) {
        self.filtered_tree_items = None;
        if let Some(opened) = self.opened_before_search.take() {
            self.tree_state.close_all();
            for path in opened {
                self.tree_state.open(path);
            }
        }
    }

    /// Recompute the live filter, open the retained branches so matches are
    /// visible, and move the selection to the first match.
    fn update_tree_filter(&mut self) {
        let query = self.search_query.to_ascii_lowercase();
        if query.is_empty() {
            self.filtered_tree_items = None;
            self.search_matches.clear();
            self.search_index = None;
            return;
        }
        match filter_tree(&self.tree_items, &query) {
            Ok(items) => {
                let paths = collect_open_paths(&items);
                self.filtered_tree_items = Some(items);
                for path in paths {
                    self.tree_state.open(path);
                }
            }
            Err(error) => {
                self.filtered_tree_items = None;
                self.content_message = Some(format!("Could not filter tree: {error}"));
                return;
            }
        }
        self.update_search_matches();
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

    pub fn start_content_search(&mut self) {
        if self.search_active || !self.search_query.is_empty() {
            self.cancel_search();
        }
        if self.content_search_active || !self.content_search_query.is_empty() {
            self.cancel_content_search();
        }
        self.content_search_active = true;
        self.content_search_query.clear();
        self.content_search_matches.clear();
        self.content_search_index = None;
        self.filtered_tree_items = None;
        if self.opened_before_search.is_none() {
            self.opened_before_search = Some(self.tree_state.opened().iter().cloned().collect());
        }
    }

    pub fn content_search_input_char(&mut self, character: char) {
        if self.content_search_query.chars().count() >= MAX_CONTENT_SEARCH_QUERY_CHARS {
            return;
        }
        self.content_search_query.push(character);
        self.submit_content_search();
    }

    pub fn content_search_backspace(&mut self) {
        self.content_search_query.pop();
        self.submit_content_search();
    }

    pub fn finish_content_search(&mut self) {
        self.content_search_active = false;
        if self.content_search_query.is_empty() {
            self.opened_before_search = None;
        }
    }

    pub fn cancel_content_search(&mut self) {
        self.content_search_active = false;
        self.content_search_request_id = self.content_search_request_id.wrapping_add(1);
        self.content_search_pending = false;
        self.content_search_query.clear();
        self.content_search_matches.clear();
        self.content_search_index = None;
        if self.content_message.as_deref().is_some_and(|message| {
            message.starts_with("No package contents match:")
                || message.starts_with("Content search failed:")
        }) {
            self.content_message = None;
        }
        self.clear_tree_filter();
    }

    pub fn has_content_search_query(&self) -> bool {
        !self.content_search_query.is_empty()
    }

    pub fn cancel_any_search(&mut self) {
        if self.content_search_active || !self.content_search_query.is_empty() {
            self.cancel_content_search();
        } else if self.search_active || !self.search_query.is_empty() {
            self.cancel_search();
        }
    }

    /// Select the next part that has an integrity issue, wrapping around.
    /// Returns `false` and reports why in the status bar when there is nothing
    /// to jump to.
    pub fn next_integrity_issue(&mut self, reverse: bool) -> bool {
        let mut paths: Vec<String> = self
            .index()
            .integrity
            .iter()
            .filter_map(|issue| issue.part.clone())
            .filter(|path| self.index().parts.contains_key(path))
            .collect();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            let total = self.index().integrity.len();
            self.status_message = Some(if total == 0 {
                "No package integrity issues".to_string()
            } else {
                format!("{total} package-level integrity issue(s); no part to jump to")
            });
            return false;
        }

        // Jump relative to the current position rather than to the ends of the
        // issue list, so a part sitting between two issues moves to the nearer
        // one in either direction.
        let current = self
            .tree_state
            .selected()
            .last()
            .cloned()
            .unwrap_or_default();
        let index = if reverse {
            let after = paths.partition_point(|path| path.as_str() < current.as_str());
            (after + paths.len() - 1) % paths.len()
        } else {
            paths.partition_point(|path| path.as_str() <= current.as_str()) % paths.len()
        };

        // An applied search filter could hide the destination, and a jump that
        // selects an invisible item is useless.
        self.cancel_any_search();
        self.status_message = None;
        self.select_path(&paths[index].clone());
        true
    }

    pub fn next_content_search_match(&mut self, reverse: bool) {
        if self.content_search_matches.is_empty() {
            return;
        }
        let current = self.content_search_index.unwrap_or(0);
        let next = if reverse {
            if current == 0 {
                self.content_search_matches.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % self.content_search_matches.len()
        };
        self.content_search_index = Some(next);
        let path = self.content_search_matches[next].clone();
        self.select_path(&path);
    }

    fn submit_content_search(&mut self) {
        self.content_search_request_id = self.content_search_request_id.wrapping_add(1);
        self.content_search_pending = false;
        self.content_search_matches.clear();
        self.content_search_index = None;
        self.filtered_tree_items = None;
        if self.content_search_query.is_empty() {
            return;
        }
        let Some((package_source, index)) = self
            .package
            .as_ref()
            .map(|package| (package.source.clone(), Arc::clone(&package.index)))
        else {
            self.content_message = Some("Package is still loading".to_string());
            return;
        };
        let request_id = self.content_search_request_id;
        if let Err(error) = self.worker.submit(Job::SearchContent {
            request_id,
            package_path: package_source,
            query: self.content_search_query.clone(),
            index,
        }) {
            self.content_message = Some(format!("Content search failed: {error}"));
            return;
        }
        self.content_search_pending = true;
        self.content_message = Some(format!(
            "Searching package contents for: {}…",
            self.content_search_query
        ));
    }

    fn apply_content_search_filter(&mut self, paths: &[String]) {
        let matches = paths.iter().collect::<HashSet<_>>();
        match filter_tree_matches(&self.tree_items, &matches) {
            Ok(items) => {
                let open_paths = collect_open_paths(&items);
                self.filtered_tree_items = Some(items);
                for path in open_paths {
                    self.tree_state.open(path);
                }
            }
            Err(error) => {
                self.filtered_tree_items = None;
                self.content_message = Some(format!("Could not filter tree: {error}"));
            }
        }
    }

    pub fn selection_status(&self) -> String {
        if self.export_active {
            return format!(
                "Extract to: {}_ | Enter save, Esc cancel",
                self.export_query
            );
        }
        let Some(selected) = self.tree_state.selected().last() else {
            let status = if self.content_search_active {
                format!(
                    "Content search: {}_ | {} matches | Enter finish, Esc cancel",
                    self.content_search_query,
                    self.content_search_matches.len()
                )
            } else if self.search_active {
                format!(
                    "Search: {}_ | {} matches | Enter select, Esc cancel",
                    self.search_query,
                    self.search_matches.len()
                )
            } else {
                "No package part selected".to_string()
            };
            return self.with_status_message(status);
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
        if self.content_search_active {
            status.push_str(&format!(
                " | Content search: {}_ | {} matches",
                self.content_search_query,
                self.content_search_matches.len()
            ));
        } else if !self.content_search_query.is_empty() {
            status.push_str(&format!(
                " | Content search: {} (n/N next, Esc clear)",
                self.content_search_query
            ));
        } else if self.search_active {
            status.push_str(&format!(" | Search: {}", self.search_query));
        } else if !self.search_query.is_empty() {
            status.push_str(&format!(
                " | Search: {} (n/N next, Esc clear)",
                self.search_query
            ));
        }
        self.with_status_message(status)
    }

    /// Export feedback has no other visible surface. Appending it last keeps it
    /// visible even when no tree item is selected.
    fn with_status_message(&self, mut status: String) -> String {
        if let Some(message) = self.status_message.as_deref() {
            status.push_str(" | ");
            status.push_str(message);
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
        self.previewed_path = None;
        self.summary_visible = false;
        self.summary_scroll = 0;
        self.status_message = None;
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

    /// The selected part when it can actually be read out of the package.
    fn selected_exportable_part(&self) -> Option<PartInfo> {
        let selected = self.tree_state.selected().last()?;
        let part = self.index().parts.get(selected)?;
        (part.kind != PartKind::Directory).then(|| part.clone())
    }

    /// Begin the extract prompt, pre-filled with the part's file name so the
    /// common case is Enter only.
    pub fn start_extract(&mut self) {
        if self.selected_exportable_part().is_none() {
            self.status_message = Some("Select a package part to extract".to_string());
            return;
        }
        if let Some(selected) = self.tree_state.selected().last() {
            self.export_query = selected.rsplit('/').next().unwrap_or(selected).to_string();
        }
        self.export_active = true;
    }

    pub fn export_input_char(&mut self, character: char) {
        if self.export_query.chars().count() >= MAX_EXPORT_PATH_CHARS {
            return;
        }
        self.export_query.push(character);
    }

    pub fn export_backspace(&mut self) {
        self.export_query.pop();
    }

    pub fn cancel_extract(&mut self) {
        self.export_active = false;
        self.export_query.clear();
    }

    pub fn confirm_extract(&mut self) -> io::Result<()> {
        self.export_active = false;
        let destination = self.export_query.trim().to_string();
        self.export_query.clear();
        if destination.is_empty() {
            self.status_message = Some("Extraction path is empty".to_string());
            return Ok(());
        }
        self.submit_export(ExportMode::SaveTo(PathBuf::from(destination)))
    }

    /// Write the selected part to a temporary file and hand it to `$PAGER`/`$EDITOR`.
    pub fn open_selected_externally(&mut self) -> io::Result<()> {
        self.submit_export(ExportMode::OpenTemp)
    }

    /// Copy the pretty-printed preview text of the selected part as OSC 52.
    pub fn copy_selected_content(&mut self) -> io::Result<()> {
        self.submit_export(ExportMode::Clipboard)
    }

    /// Hand a finished export to the event loop, which owns the terminal.
    pub fn take_pending_export(&mut self) -> Option<PendingExport> {
        self.pending_export.take()
    }

    fn submit_export(&mut self, mode: ExportMode) -> io::Result<()> {
        let Some(part) = self.selected_exportable_part() else {
            self.status_message = Some("Select a package part to export".to_string());
            return Ok(());
        };
        let Some((package_source, index)) = self
            .package
            .as_ref()
            .map(|package| (package.source.clone(), Arc::clone(&package.index)))
        else {
            self.status_message = Some("Package is still loading".to_string());
            return Ok(());
        };
        self.export_request_id = self.export_request_id.wrapping_add(1);
        let request_id = self.export_request_id;
        if let Err(error) = self.worker.submit(Job::ExportPart {
            request_id,
            package_path: package_source,
            part: Box::new(part),
            index,
            mode,
        }) {
            self.export_pending = false;
            self.status_message = Some(format!("Export failed: {error}"));
            return Ok(());
        }
        self.export_pending = true;
        self.status_message = Some("Preparing export…".to_string());
        Ok(())
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
        // Parts with an integrity issue are marked in the tree so they can be
        // spotted without reading the metadata panel.
        let issue_parts: HashSet<String> = self
            .index()
            .integrity
            .iter()
            .filter_map(|issue| issue.part.clone())
            .collect();
        self.filtered_tree_items = None;
        self.opened_before_search = None;
        match create_tree(&paths, &issue_parts) {
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
fn create_tree(
    paths: &[String],
    issue_parts: &HashSet<String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    create_tree_level("", paths, 0, issue_parts)
}

fn collect_open_paths(items: &[TreeItem<'static, String>]) -> Vec<Vec<String>> {
    fn collect(
        items: &[TreeItem<'static, String>],
        parent: &[String],
        paths: &mut Vec<Vec<String>>,
    ) {
        for item in items {
            let mut path = parent.to_vec();
            path.push(item.identifier().clone());
            if !item.children().is_empty() {
                paths.push(path.clone());
                collect(item.children(), &path, paths);
            }
        }
    }

    let mut paths = Vec::new();
    collect(items, &[], &mut paths);
    paths
}

/// Keep items whose path matches `query`, retaining ancestor directories so
/// matches stay reachable. An item that matches directly keeps its whole
/// subtree. Item text is the final path component, so branches can be rebuilt
/// without access to the original (crate-private) text.
fn filter_tree(
    items: &[TreeItem<'static, String>],
    query: &str,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut result = Vec::new();
    for item in items {
        if item.identifier().to_ascii_lowercase().contains(query) {
            result.push(item.clone());
            continue;
        }
        if item.children().is_empty() {
            continue;
        }
        let children = filter_tree(item.children(), query)?;
        if children.is_empty() {
            continue;
        }
        let name = item
            .identifier()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        // Children are a subset of a valid sibling set, so identifiers stay unique.
        let branch =
            TreeItem::new(item.identifier().clone(), name, children).map_err(io::Error::other)?;
        result.push(branch);
    }
    Ok(result)
}

fn filter_tree_matches(
    items: &[TreeItem<'static, String>],
    matches: &HashSet<&String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut result = Vec::new();
    for item in items {
        if matches.contains(item.identifier()) {
            result.push(item.clone());
            continue;
        }
        if item.children().is_empty() {
            continue;
        }
        let children = filter_tree_matches(item.children(), matches)?;
        if children.is_empty() {
            continue;
        }
        let name = item
            .identifier()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let branch =
            TreeItem::new(item.identifier().clone(), name, children).map_err(io::Error::other)?;
        result.push(branch);
    }
    Ok(result)
}

/// Tree labels for parts with an integrity issue carry a marker so they stand
/// out without opening the metadata panel.
fn tree_label(head: &str, identifier: &str, issue_parts: &HashSet<String>) -> String {
    if issue_parts.contains(identifier) {
        format!("{head} ⚠")
    } else {
        head.to_string()
    }
}

/// `offset` is the byte length of the shared ancestor prefix including its
/// trailing slash, so recursion never re-allocates path components. Paths are
/// sorted, which groups a directory's children contiguously after it.
fn create_tree_level(
    parent: &str,
    paths: &[String],
    offset: usize,
    issue_parts: &HashSet<String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut items = Vec::new();
    let mut index = 0;
    while index < paths.len() {
        let rest = &paths[index][offset..];
        let head = rest.split('/').next().unwrap_or(rest);
        let identifier = format!("{parent}/{head}");
        let label = tree_label(head, &identifier, issue_parts);
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
            items.push(TreeItem::new_leaf(identifier, label));
        } else {
            let child_offset = offset + head.len() + 1;
            let children = create_tree_level(&identifier, children, child_offset, issue_parts)?;
            items.push(TreeItem::new(identifier, label, children).map_err(io::Error::other)?);
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::PendingExport;
    use crate::preview::PreviewKind;
    use crate::{App, worker::Worker};
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
        assert!(
            summary
                .links
                .iter()
                .any(|link| link.target == "/ppt/slides/slide1.xml")
        );

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
        assert!(
            slide
                .content_type
                .as_deref()
                .is_some_and(|content_type| { content_type.contains("presentationml.slide+xml") })
        );

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

    /// Place the editor cursor on the first occurrence of `needle`.
    fn put_cursor_on(app: &mut App, needle: &str) -> bool {
        for (row, line) in app.editor_state.lines.to_vecs().iter().enumerate() {
            let text: String = line.iter().collect();
            if let Some(byte_offset) = text.find(needle) {
                let column = text[..byte_offset].chars().count();
                app.editor_state.cursor = edtui::Index2::new(row, column);
                return true;
            }
        }
        false
    }

    /// Issue #14 acceptance: an `r:embed="rIdN"` token in slide XML opens the
    /// referenced image part, `Alt-Left` returns to the slide, and an external
    /// hyperlink reference is reported in the status bar instead of followed.
    #[test]
    fn relationship_references_jump_to_their_targets() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        let selected = |app: &App| app.tree_state.selected().last().cloned();
        app.select_path("/ppt/slides/slide2.xml");
        app.load_selected_file_content()?;
        preview_loaded(&mut app);
        assert_eq!(app.preview_kind, PreviewKind::Xml);

        // Ordinary XML at the cursor is not a reference.
        app.editor_state.cursor = edtui::Index2::new(0, 0);
        assert!(!app.follow_relationship_at_cursor()?);
        assert!(app.status_message.is_none());

        assert!(put_cursor_on(&mut app, "\"rId2\""), "slide embeds rId2");
        assert!(app.follow_relationship_at_cursor()?);
        preview_loaded(&mut app);
        assert_eq!(selected(&app).as_deref(), Some("/ppt/media/image1.gif"));
        assert!(app.image_state.is_some());

        assert!(app.navigate_back()?);
        preview_loaded(&mut app);
        assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide2.xml"));

        assert!(put_cursor_on(&mut app, "r:id=\"rId3\""));
        assert!(app.follow_relationship_at_cursor()?);
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("https://chunyu.site/neovim/"))
        );
        assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide2.xml"));

        // The tree selection may move on without reloading the preview; tokens
        // must still resolve against the part shown in the editor, not the
        // selection. slide1 (the selection) has no rId2, so a selection-based
        // lookup would fail to jump at all.
        app.tree_state
            .select(vec!["/ppt/slides/slide1.xml".to_string()]);
        assert!(put_cursor_on(&mut app, "r:embed=\"rId2\""));
        assert!(app.follow_relationship_at_cursor()?);
        preview_loaded(&mut app);
        assert_eq!(selected(&app).as_deref(), Some("/ppt/media/image1.gif"));
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
    fn jumping_to_issues_reports_when_there_are_none() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        assert!(app.index().integrity.is_empty());
        assert!(!app.next_integrity_issue(false));
        assert_eq!(
            app.status_message.as_deref(),
            Some("No package integrity issues")
        );
        assert!(
            app.selection_status()
                .contains("No package integrity issues")
        );
        Ok(())
    }

    /// A package with a dangling relationship target and a part without a
    /// content type. Returns its path so the caller can delete it afterwards.
    fn write_test_package(name: &str) -> io::Result<std::path::PathBuf> {
        use std::io::Write as _;

        let path = std::env::temp_dir().join(format!("oox-test-{}-{name}", std::process::id()));
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path)?);
        let entries = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#,
            ),
            ("ppt/presentation.xml", "<p:presentation/>"),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../p.xml"/>
</Relationships>"#,
            ),
            ("ppt/notes.txt", "notes"),
            ("ppt/p.xml", "<p/>"),
            ("ppt/aaa.txt", "aaa"),
        ];
        for (entry, content) in entries {
            writer
                .start_file(entry, zip::write::SimpleFileOptions::default())
                .map_err(io::Error::other)?;
            writer.write_all(content.as_bytes())?;
        }
        writer.finish().map_err(io::Error::other)?;
        Ok(path)
    }

    /// Acceptance for issue #12 phase 1: a dangling relationship target is
    /// reported, marked in the tree, and reachable from the issue list.
    #[test]
    fn integrity_issues_mark_the_tree_and_are_navigable() -> io::Result<()> {
        let path = write_test_package("integrity.pptx")?;
        let mut app = test_app(&path.to_string_lossy())?;

        let issues = &app.index().integrity;
        assert!(issues.iter().any(|issue| {
            issue.part.as_deref() == Some("/ppt/presentation.xml")
                && issue.message.contains("/ppt/slides/slide1.xml")
        }));
        assert!(issues.iter().any(|issue| {
            issue.part.as_deref() == Some("/ppt/notes.txt")
                && issue.message.contains("no content type")
        }));

        // `i` walks the offending parts and clears the stale status message.
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.status_message = Some("old".to_string());
        assert!(app.next_integrity_issue(false));
        assert_eq!(app.status_message, None);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/aaa.txt")
        );

        // The issue list in the metadata panel links to the offending part.
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        let link = app
            .details_view()
            .links
            .iter()
            .find(|link| link.target == "/ppt/presentation.xml")
            .cloned()
            .expect("the issue list should link to the offending part");
        assert!(app.activate_detail_link(link.line, link.start)?);
        assert_eq!(
            app.tree_state.selected().last().map(String::as_str),
            Some("/ppt/presentation.xml")
        );
        preview_loaded(&mut app);

        let issue_parts: std::collections::HashSet<String> = app
            .index()
            .integrity
            .iter()
            .filter_map(|issue| issue.part.clone())
            .collect();
        assert_eq!(
            super::tree_label("presentation.xml", "/ppt/presentation.xml", &issue_parts),
            "presentation.xml ⚠"
        );
        assert_eq!(
            super::tree_label("presentation.xml", "/ppt/other.xml", &issue_parts),
            "presentation.xml"
        );

        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// Issue navigation must be directional from any tree position, not just
    /// from one of the list ends, and must not leave the destination hidden
    /// behind an applied search filter.
    #[test]
    fn issue_jumps_are_directional_and_clear_the_search_filter() -> io::Result<()> {
        let path = write_test_package("integrity-direction.pptx")?;
        let mut app = test_app(&path.to_string_lossy())?;
        fn selected(app: &App) -> String {
            app.tree_state
                .selected()
                .last()
                .cloned()
                .unwrap_or_default()
        }

        // `/ppt/p.xml` has no issue and sorts between two that do.
        assert!(app.index().parts.contains_key("/ppt/p.xml"));
        app.select_path("/ppt/p.xml");
        assert!(app.next_integrity_issue(false));
        assert_eq!(selected(&app), "/ppt/presentation.xml");

        app.select_path("/ppt/p.xml");
        assert!(app.next_integrity_issue(true));
        assert_eq!(selected(&app), "/ppt/notes.txt");

        // Wrap around at the ends.
        app.select_path("/ppt/presentation.xml");
        assert!(app.next_integrity_issue(false));
        assert_eq!(selected(&app), "/ppt/aaa.txt");
        app.select_path("/ppt/aaa.txt");
        assert!(app.next_integrity_issue(true));
        assert_eq!(selected(&app), "/ppt/presentation.xml");

        // A filter that hides the next issue is dropped so the jump is visible.
        app.start_search();
        for character in "aaa.txt".chars() {
            app.search_input_char(character);
        }
        app.finish_search();
        assert!(app.tree_filter_active());
        assert!(app.next_integrity_issue(false));
        assert!(!app.tree_filter_active());
        assert_eq!(selected(&app), "/ppt/notes.txt");
        assert!(
            flatten_identifiers(app.visible_tree_items()).contains(&"/ppt/notes.txt".to_string())
        );

        // Activating a link does the same for its destination.
        app.start_search();
        for character in "aaa.txt".chars() {
            app.search_input_char(character);
        }
        app.finish_search();
        app.select_path("/ppt/aaa.txt");
        let link = app
            .details_view()
            .links
            .iter()
            .find(|link| link.target == "/ppt/notes.txt")
            .cloned()
            .expect("the issue list should link to the other issue part");
        assert!(app.activate_detail_link(link.line, link.start)?);
        assert!(!app.tree_filter_active());
        assert_eq!(selected(&app), "/ppt/notes.txt");
        assert!(
            flatten_identifiers(app.visible_tree_items()).contains(&"/ppt/notes.txt".to_string())
        );
        preview_loaded(&mut app);

        std::fs::remove_file(&path)?;
        Ok(())
    }

    fn flatten_identifiers(items: &[tui_tree_widget::TreeItem<'static, String>]) -> Vec<String> {
        let mut result = Vec::new();
        for item in items {
            result.push(item.identifier().clone());
            result.extend(flatten_identifiers(item.children()));
        }
        result
    }

    #[test]
    fn search_filters_tree_live_while_typing() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        let full_tree = flatten_identifiers(&app.tree_items);
        assert!(full_tree.contains(&"/docProps/core.xml".to_string()));

        app.start_search();
        for character in "slide1.xml".chars() {
            app.search_input_char(character);
        }

        // Live, before Enter: non-matching paths are hidden, ancestors of
        // matches are retained, and the selection already follows the filter.
        let visible = flatten_identifiers(app.visible_tree_items());
        assert!(visible.contains(&"/ppt/slides/slide1.xml".to_string()));
        assert!(visible.contains(&"/ppt".to_string()));
        assert!(visible.contains(&"/ppt/slides".to_string()));
        assert!(!visible.contains(&"/docProps/core.xml".to_string()));
        assert!(visible.len() < full_tree.len());
        // Selection follows the first sorted match (the slide's .rels sorts first).
        let first_match = app.search_matches.first().cloned();
        assert_eq!(
            app.tree_state.selected().last().cloned().as_ref(),
            first_match.as_ref()
        );
        assert!(app.tree_filter_active());

        // Backspacing to an empty query restores the full tree view.
        for _ in 0.."slide1.xml".chars().count() {
            app.search_backspace();
        }
        assert!(!app.tree_filter_active());
        assert_eq!(flatten_identifiers(app.visible_tree_items()), full_tree);
        Ok(())
    }

    #[test]
    fn finish_search_keeps_filter_and_cancel_restores_tree_state() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        let opened_before: Vec<Vec<String>> = app.tree_state.opened().iter().cloned().collect();

        app.start_search();
        for character in "slideLayouts".chars() {
            app.search_input_char(character);
        }
        app.finish_search();

        // The filter stays applied after Enter so n/N can cycle the matches.
        assert!(!app.search_active);
        assert!(app.tree_filter_active());
        // Every retained path either matches the query or is an ancestor of a match.
        let visible = flatten_identifiers(app.visible_tree_items());
        assert!(!visible.is_empty());
        for path in &visible {
            let is_ancestor_of_match = visible
                .iter()
                .any(|other| other.starts_with(&format!("{path}/")));
            assert!(
                path.contains("slideLayouts") || is_ancestor_of_match,
                "unexpected path in filtered tree: {path}"
            );
        }

        // Esc (cancel) restores the full tree and the pre-search open state.
        app.cancel_search();
        assert!(!app.tree_filter_active());
        let opened_after: Vec<Vec<String>> = app.tree_state.opened().iter().cloned().collect();
        assert_eq!(opened_before.len(), opened_after.len());
        assert!(app.search_query.is_empty());
        Ok(())
    }

    #[test]
    fn content_search_runs_in_background_and_filters_matching_parts() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.start_content_search();
        for character in "OOXML TUI".chars() {
            app.content_search_input_char(character);
        }
        pump_until(&mut app, |app| !app.content_search_pending);

        assert!(app.content_search_active);
        assert!(
            app.content_search_matches
                .iter()
                .any(|path| path == "/ppt/slides/slide1.xml")
        );
        assert!(app.tree_filter_active());
        assert!(
            app.visible_tree_items()
                .iter()
                .any(|item| item.identifier() == "/ppt")
        );

        app.finish_content_search();
        app.next_content_search_match(false);
        app.cancel_content_search();
        assert!(!app.tree_filter_active());
        assert!(app.content_search_query.is_empty());
        Ok(())
    }

    #[test]
    fn extract_writes_a_byte_identical_part() -> io::Result<()> {
        use std::io::Read as _;

        let mut app = test_app("data/sample.pptx")?;
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.start_extract();
        assert_eq!(app.export_query, "[Content_Types].xml");

        let destination = std::env::temp_dir().join(format!(
            "oox-test-extract-{}-{}.xml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        app.export_query = destination.to_string_lossy().into_owned();
        app.confirm_extract()?;
        pump_until(&mut app, |app| !app.export_pending);

        let Some(PendingExport::Extracted(path)) = app.take_pending_export() else {
            panic!("expected an extracted file");
        };
        assert_eq!(path, destination);

        let mut expected = Vec::new();
        zip::ZipArchive::new(std::fs::File::open("data/sample.pptx")?)?
            .by_name("[Content_Types].xml")?
            .read_to_end(&mut expected)?;
        assert_eq!(std::fs::read(&path)?, expected);

        // Extracting again refuses to clobber the file that was just written.
        app.select_path("/[Content_Types].xml");
        app.export_query = path.to_string_lossy().into_owned();
        app.confirm_extract()?;
        pump_until(&mut app, |app| !app.export_pending);
        assert!(app.take_pending_export().is_none());
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("Export failed"))
        );

        std::fs::remove_file(&path)?;
        Ok(())
    }

    #[test]
    fn copy_produces_pretty_printed_text_for_xml_parts() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.tree_state
            .select(vec!["/[Content_Types].xml".to_string()]);
        app.copy_selected_content()?;
        pump_until(&mut app, |app| !app.export_pending);

        match app.take_pending_export() {
            Some(PendingExport::Clipboard(text)) => {
                assert!(text.contains("<Types"));
                assert!(text.contains("\n  "), "expected indented XML, got: {text}");
            }
            _ => panic!("expected clipboard text"),
        }
        Ok(())
    }

    #[test]
    fn status_message_is_visible_without_a_selection() -> io::Result<()> {
        let mut app = test_app("data/sample.pptx")?;
        app.tree_state.select(Vec::new());
        app.status_message = Some("Export failed: boom".to_string());
        assert!(app.selection_status().contains("Export failed: boom"));
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
        let items = super::create_tree(&paths, &Default::default())?;
        assert_eq!(items.len(), 2);
        let ppt = &items[1];
        assert_eq!(ppt.identifier(), "/ppt");
        assert_eq!(ppt.children().len(), 1);
        assert_eq!(ppt.children()[0].identifier(), "/ppt/slides");
        Ok(())
    }
}
