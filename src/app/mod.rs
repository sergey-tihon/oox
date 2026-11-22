use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use edtui::{EditorState, Lines, RowIndex};
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use tui_tree_widget::{TreeItem, TreeState};

use crate::compare::{Comparison, PartStatus};
use crate::keybindings::{Action, key_hint};
use crate::package::{
    Diagnostic, DiagnosticSeverity, Package, PackageIndex, PartInfo, PartKind, Relationship,
    TargetMode, is_image_name, is_xml_name,
};
use crate::preview::{Preview, PreviewKind};
use crate::summary::{DetailLink, DetailsView};
use crate::worker::{
    ExportMode, ExportOutcome, Job, ResultMessage, TempPart, Worker, accepts_result,
};
mod details;
mod edit;
mod export;
mod navigation;
mod save;
mod search;
mod tree;
use details::{DetailsPanel, SummaryPanel};
use edit::EditorSession;
use export::ExportFlow;
use navigation::NavigationHistory;
use save::SaveFlow;
use search::{ContentSearch, PathSearch};
use tree::{TreeView, collect_open_paths, filter_tree, filter_tree_matches, tree_contains};

/// Rows scanned in each direction when locating the start tag around the editor
/// cursor. A start tag split across more rows than this is not worth following.
const MAX_TAG_ROWS: usize = 64;

/// Work an export produced that only the event loop can finish: running a
/// command needs the terminal, and OSC 52 needs the backend's writer.
pub enum PendingExport {
    Extracted(PathBuf),
    OpenTemp(TempPart),
    Clipboard(String),
}

/// A destructive action waiting for a deliberate second press. Held as one
/// value so any unrelated key (or a real mouse button, but not plain motion)
/// disarms the pending question.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Confirmation {
    Quit,
    Revert(String),
}

/// Where an editor cursor was before its buffer was replaced, so a reload does
/// not throw the user back to the top of the file or out of insert mode.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorSpot {
    cursor: edtui::Index2,
    viewport: (usize, usize),
    mode: edtui::EditorMode,
}

impl CursorSpot {
    fn of(state: &EditorState) -> Self {
        Self {
            cursor: state.cursor,
            viewport: state.viewport_offset(),
            mode: state.mode,
        }
    }
}

/// The part to select once a saved package is re-indexed, and where its cursor was.
#[derive(Clone, Debug)]
pub(crate) struct Reselect {
    path: String,
    spot: CursorSpot,
}

/// Put a cursor, viewport and mode back after the buffer was replaced, clamped
/// to the new buffer with edtui's bounds for that mode: insert mode may sit one
/// past the last column (and on the row after the last line), normal mode may
/// not. Visual and search mode fall back to normal, because their selection and
/// query are not carried over and restoring the bare mode would be inconsistent.
pub(super) fn place_cursor(state: &mut EditorState, spot: CursorSpot) {
    let insert = spot.mode == edtui::EditorMode::Insert;
    let last_row = if insert {
        state.lines.len()
    } else {
        state.lines.len().saturating_sub(1)
    };
    let row = spot.cursor.row.min(last_row);
    let columns = state.lines.len_col(row).unwrap_or(0);
    let last_col = if insert {
        columns
    } else {
        columns.saturating_sub(1)
    };
    state.mode = if insert {
        edtui::EditorMode::Insert
    } else {
        edtui::EditorMode::Normal
    };
    state.cursor = edtui::Index2::new(row, spot.cursor.col.min(last_col));
    state.set_viewport_offset(spot.viewport.0, spot.viewport.1);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurrentWidget {
    Tree,
    Details,
    TextArea,
}

/// A second package opened alongside the primary one, with the part-level
/// comparison computed by the worker.
pub struct Compare {
    pub package: Package,
    pub comparison: Comparison,
}

pub(crate) struct PreviewState {
    pub(crate) kind: PreviewKind,
    pub(crate) image_state: Option<StatefulProtocol>,
    pub(crate) picker: Picker,
    pub(crate) request_id: u64,
    pub(crate) pending: bool,
}

pub struct App {
    pub(crate) file_path: String,
    pub(crate) tree: TreeView,
    pub(crate) search: PathSearch,
    pub(crate) content_search: ContentSearch,
    pub(crate) editor: EditorSession,
    pub(crate) save: SaveFlow,
    pub(crate) export: ExportFlow,
    pub(crate) details: DetailsPanel,
    pub(crate) summary: SummaryPanel,
    pub(crate) history: NavigationHistory,
    pub(crate) preview: PreviewState,
    pub(crate) current_widget: CurrentWidget,
    pub(crate) content_message: Option<String>,
    pub(crate) package: Option<Package>,
    pub(crate) compare: Option<Compare>,
    pub(crate) hide_unchanged_parts: bool,
    pub(crate) compare_path: Option<PathBuf>,
    pub(crate) worker: Worker,
    pub(crate) open_request_id: u64,
    pub(crate) loading: bool,
    pub(crate) worker_error: Option<String>,
    pub(crate) show_help: bool,
    pub(crate) pending_confirmation: Option<Confirmation>,
    pub(crate) status_message: Option<String>,
}
impl App {
    /// Construct an interactive loading state without opening the archive on the UI thread.
    pub fn new_loading(
        path: String,
        compare_path: Option<PathBuf>,
        picker: Picker,
        worker: Worker,
    ) -> io::Result<Self> {
        let clipboard = crate::clipboard::shared();
        let mut editor_state = EditorState::default();
        editor_state.set_clipboard(clipboard.clone());

        let app = Self {
            file_path: path.clone(),
            tree: TreeView {
                state: TreeState::default(),
                items: Vec::new(),
                filtered: None,
                opened_before_search: None,
            },
            search: PathSearch {
                active: false,
                query: String::new(),
                matches: Vec::new(),
                index: None,
            },
            content_search: ContentSearch {
                active: false,
                query: String::new(),
                matches: Vec::new(),
                index: None,
                request_id: 0,
                pending: false,
            },
            editor: EditorSession {
                state: editor_state,
                clipboard,
                baseline: None,
                editable: false,
                dirty: false,
                edits: BTreeMap::new(),
                previewed_path: None,
                external_edit_request: None,
            },
            save: SaveFlow {
                active: false,
                query: String::new(),
                confirm: None,
                warnings: Vec::new(),
                request_id: 0,
                pending: false,
                reloading: false,
                reload_preview_request_id: None,
                last_saved: None,
                pending_reselect: None,
                pending_cursor: None,
                post_load_message: None,
            },
            export: ExportFlow {
                active: false,
                query: String::new(),
                request_id: 0,
                pending: false,
                ready: None,
            },
            details: DetailsPanel {
                visible: true,
                scroll: 0,
                cursor: 0,
                cache: DetailsView::default(),
                cache_key: None,
                generation: 0,
            },
            summary: SummaryPanel {
                view: None,
                visible: false,
                scroll: 0,
            },
            history: NavigationHistory {
                back: VecDeque::new(),
                forward: Vec::new(),
                current: None,
            },
            preview: PreviewState {
                kind: PreviewKind::Empty,
                image_state: None,
                picker,
                request_id: 0,
                pending: false,
            },
            compare_path: compare_path.clone(),
            current_widget: CurrentWidget::Tree,
            content_message: Some("Loading package…".to_string()),
            package: None,
            compare: None,
            hide_unchanged_parts: false,
            worker,
            open_request_id: 1,
            loading: true,
            worker_error: None,
            show_help: false,
            pending_confirmation: None,
            status_message: None,
        };
        app.worker.submit(match app.compare_path.clone() {
            Some(compare_path) => Job::Compare {
                request_id: app.open_request_id,
                path: PathBuf::from(&app.file_path),
                compare_path,
            },
            None => Job::Open {
                request_id: app.open_request_id,
                path: PathBuf::from(&app.file_path),
            },
        })?;
        Ok(app)
    }

    /// A fresh editor buffer wired to the shared clipboard.
    pub(super) fn new_editor_state(&self, lines: Lines) -> EditorState {
        let mut state = EditorState::new(lines);
        state.set_clipboard(self.editor.clipboard.clone());
        state
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
                    self.preview.pending = false;
                    self.content_search.pending = false;
                    self.worker_error = Some(error.to_string());
                    self.content_message = Some(format!("Package worker failed: {error}"));
                    self.save.pending = false;
                    self.save.reloading = false;
                    self.save.reload_preview_request_id = None;
                    self.save.pending_reselect = None;
                    self.save.pending_cursor = None;
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
                        Ok(package) => self.install_loaded(package, summary.view, None),
                        Err(error) => self.fail_load(error),
                    }
                }
                ResultMessage::Compared {
                    request_id,
                    path,
                    compare_path,
                    result,
                } => {
                    if request_id != self.open_request_id
                        || path.to_string_lossy() != self.file_path
                    {
                        continue;
                    }
                    self.compare_path = Some(compare_path);
                    match *result {
                        Ok(payload) => {
                            let compare = Compare {
                                package: payload.b,
                                comparison: payload.comparison,
                            };
                            self.install_loaded(payload.a, payload.summary.view, Some(compare));
                        }
                        Err(error) => self.fail_load(error),
                    }
                }
                ResultMessage::PartRead {
                    request_id,
                    selected_path,
                    preview,
                } => {
                    let completes_save_reload =
                        self.save.reload_preview_request_id == Some(request_id);
                    if request_id == self.preview.request_id {
                        self.preview.pending = false;
                    }
                    let current = self.tree.state.selected().last().cloned();
                    if !current.as_deref().is_some_and(|path| {
                        accepts_result(request_id, self.preview.request_id, &selected_path, path)
                    }) {
                        if completes_save_reload {
                            self.save.reloading = false;
                            self.save.reload_preview_request_id = None;
                            self.save.pending_cursor = None;
                        }
                        continue;
                    }
                    // A cursor restore is only valid for the part it was taken
                    // from; any other preview just drops it.
                    let restore = self.save.pending_cursor.take();
                    match preview {
                        Ok(Preview::Editor {
                            kind,
                            text,
                            editable,
                        }) => {
                            self.preview.kind = kind;
                            let lines = Lines::from(text.as_str());
                            self.editor.state = self.new_editor_state(lines.clone());
                            if let Some(reselect) = restore
                                && reselect.path == selected_path
                            {
                                place_cursor(&mut self.editor.state, reselect.spot);
                            }
                            self.editor.baseline = Some(lines);
                            self.editor.editable = editable;
                            self.editor.dirty = false;
                            self.editor.previewed_path = Some(selected_path);
                            self.content_message = None;
                        }
                        Ok(Preview::Image(image)) => {
                            self.preview.kind = PreviewKind::Image;
                            self.preview.image_state =
                                Some(self.preview.picker.new_resize_protocol(image));
                            self.content_message = None;
                            self.forget_external_edit(&selected_path);
                        }
                        Ok(Preview::Info(message)) => {
                            self.preview.kind = PreviewKind::Info;
                            self.content_message = Some(message);
                            self.forget_external_edit(&selected_path);
                        }
                        Ok(Preview::Error(message)) => {
                            self.preview.kind = PreviewKind::Error;
                            self.content_message = Some(format!(
                                "Could not preview {}: {message}",
                                selected_path.trim_start_matches('/')
                            ));
                            self.forget_external_edit(&selected_path);
                        }
                        Err(error) => {
                            self.preview.kind = PreviewKind::Error;
                            self.content_message = Some(format!("Could not preview: {error}"));
                            self.forget_external_edit(&selected_path);
                        }
                    }
                    if completes_save_reload {
                        self.save.reloading = false;
                        self.save.reload_preview_request_id = None;
                    }
                }
                ResultMessage::ContentSearch {
                    request_id,
                    query,
                    result,
                } => {
                    if request_id != self.content_search.request_id {
                        continue;
                    }
                    self.content_search.pending = false;
                    match result {
                        Ok(result) => {
                            self.content_search.matches = result.matches;
                            self.content_search.index =
                                (!self.content_search.matches.is_empty()).then_some(0);
                            self.apply_content_search_filter();
                            if result.skipped_parts > 0 {
                                self.status_message = Some(format!(
                                    "Content search skipped {} unreadable part(s)",
                                    result.skipped_parts
                                ));
                            }
                            if let Some(path) = self.content_search.matches.first().cloned() {
                                self.select_path(&path);
                                self.content_message = None;
                            } else {
                                self.content_message =
                                    Some(format!("No package contents match: {query}"));
                            }
                        }
                        Err(error) => {
                            self.content_search.matches.clear();
                            self.content_search.index = None;
                            self.tree.filtered = Some(Vec::new());
                            self.content_message = Some(format!("Content search failed: {error}"));
                        }
                    }
                }
                ResultMessage::Exported {
                    request_id,
                    outcome,
                } => {
                    if request_id != self.export.request_id {
                        // A stale `TempPart` removes its file as it drops.
                        continue;
                    }
                    self.export.pending = false;
                    match outcome {
                        Ok(ExportOutcome::Saved(path)) => {
                            self.export.ready = Some(PendingExport::Extracted(path));
                        }
                        Ok(ExportOutcome::TempFile(temp)) => {
                            self.export.ready = Some(PendingExport::OpenTemp(temp));
                        }
                        Ok(ExportOutcome::Clipboard(text)) => {
                            self.export.ready = Some(PendingExport::Clipboard(text));
                        }
                        Err(error) => {
                            self.status_message = Some(format!("Export failed: {error}"));
                        }
                    }
                }
                ResultMessage::Saved {
                    request_id,
                    path,
                    result,
                } => {
                    if request_id != self.save.request_id {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.save.pending = false;
                            self.after_save(path);
                        }
                        Err(error) => {
                            self.save.pending = false;
                            self.status_message = Some(format!("Save failed: {error}"));
                        }
                    }
                }
            }
        }
        // Watchdog: explicit in-flight flags instead of inspecting message text.
        if !self.worker.is_alive()
            && (self.loading
                || self.preview.pending
                || self.content_search.pending
                || self.export.pending
                || self.save.pending
                || self.save.reloading)
        {
            self.loading = false;
            self.preview.pending = false;
            self.content_search.pending = false;
            self.export.pending = false;
            self.save.pending = false;
            self.save.reloading = false;
            self.save.reload_preview_request_id = None;
            let message = "Package worker exited before completing the request".to_string();
            self.worker_error = Some(message.clone());
            self.content_message = Some(message);
            changed = true;
        }
        changed
    }

    /// Install a freshly opened package (optionally with a comparison) and
    /// reset every view that referred to the previous one.
    pub(super) fn install_loaded(
        &mut self,
        package: Package,
        summary: Option<DetailsView>,
        compare: Option<Compare>,
    ) {
        self.package = Some(package);
        self.hide_unchanged_parts = compare.is_some();
        self.compare = compare;
        self.details.generation = self.details.generation.wrapping_add(1);
        self.editor.state = self.new_editor_state(Lines::default());
        self.editor.baseline = None;
        self.editor.editable = false;
        self.editor.dirty = false;
        // A freshly opened package replaces the edits of the previous one; the
        // only reload that keeps them is a save, which clears them beforehand.
        self.editor.edits.clear();
        self.preview.image_state = None;
        self.preview.kind = PreviewKind::Empty;
        self.editor.previewed_path = None;
        self.install_tree();
        if self.compare.is_some() {
            self.expand_all();
        }
        self.summary.view = summary;
        self.loading = false;
        self.content_message = Some(if self.compare.is_some() {
            "Comparing packages: select a part to see its diff".to_string()
        } else {
            "Select a package part or press Enter to preview content".to_string()
        });
        // After a save the saved part is shown again, now from the new package.
        if let Some(reselect) = self.save.pending_reselect.take()
            && self.index().parts.contains_key(&reselect.path)
        {
            self.select_path(&reselect.path);
            self.save.pending_cursor = Some(reselect);
            let _ = self.load_selected_file_content();
            if self.save.reloading && self.preview.pending {
                self.save.reload_preview_request_id = Some(self.preview.request_id);
            }
        }
        if self.save.reloading && self.save.reload_preview_request_id.is_none() {
            self.save.reloading = false;
        }
        if let Some(message) = self.save.post_load_message.take() {
            self.status_message = Some(message);
        }
    }

    fn fail_load(&mut self, error: String) {
        self.loading = false;
        self.save.reloading = false;
        self.save.reload_preview_request_id = None;
        self.save.pending_reselect = None;
        self.save.pending_cursor = None;
        self.worker_error = Some(error.clone());
        self.content_message = Some(format!("Could not open package: {error}"));
    }

    pub fn is_package_loaded(&self) -> bool {
        !self.loading && self.package.is_some()
    }
}
impl App {
    pub fn load_selected_file_content(&mut self) -> io::Result<()> {
        self.load_selected_file_content_inner(true)
    }

    pub(super) fn load_selected_file_content_inner(
        &mut self,
        record_history: bool,
    ) -> io::Result<()> {
        // Set the current buffer aside before it is replaced.
        self.stash_current_edit();
        // Reset the previous view before loading anything new. This prevents a
        // previously selected XML file or image from remaining visible when a
        // directory or unsupported package part is selected.
        self.preview.image_state = None;
        self.editor.state = self.new_editor_state(Lines::default());
        self.editor.baseline = None;
        self.editor.editable = false;
        self.editor.dirty = false;
        self.preview.kind = PreviewKind::Empty;
        self.editor.previewed_path = None;
        self.summary.visible = false;
        self.summary.scroll = 0;
        self.status_message = None;
        self.content_message = Some("Select a package part to inspect".to_string());

        let selected = match self.tree.state.selected().last().cloned() {
            Some(selected) => selected,
            None => return Ok(()),
        };
        if record_history {
            self.record_navigation(&selected);
        }
        // A pending external edit belongs to one part; moving on abandons it
        // rather than launching the editor when the part is opened again later.
        if self.editor.external_edit_request.as_deref() != Some(selected.as_str()) {
            self.editor.external_edit_request = None;
        }
        // A part that already has unsaved edits keeps them: reloading would
        // silently throw the user's work away.
        if self.restore_edit(&selected) {
            return Ok(());
        }

        let display_name = selected.trim_start_matches('/').to_string();
        // In compare mode the selected part may exist in either package, so a
        // part that was added or removed is still previewable as a diff.
        let part_a = self.index().parts.get(&selected).cloned();
        let part_b = self
            .compare
            .as_ref()
            .and_then(|compare| compare.package.index.parts.get(&selected).cloned());
        let Some(part) = [part_a, part_b]
            .into_iter()
            .flatten()
            .find(|part| part.kind != PartKind::Directory)
        else {
            let message = if self.is_directory(&selected) {
                format!("Directory: {display_name}")
            } else {
                format!("Unavailable package part: {display_name}")
            };
            self.content_message = Some(message);
            return Ok(());
        };
        let Some((package_source, index)) = self
            .package
            .as_ref()
            .map(|package| (package.source.clone(), Arc::clone(&package.index)))
        else {
            self.content_message = Some("Package is still loading".to_string());
            return Ok(());
        };
        self.preview.request_id = self.preview.request_id.wrapping_add(1);
        let job = match self.compare.as_ref() {
            Some(compare) => Job::DiffPart {
                request_id: self.preview.request_id,
                package_a: package_source,
                package_b: compare.package.source.clone(),
                part_path: selected.clone(),
                index_a: index,
                index_b: Arc::clone(&compare.package.index),
            },
            None => Job::ReadPart {
                request_id: self.preview.request_id,
                package_path: package_source,
                part: Box::new(part),
                index,
            },
        };
        if let Err(error) = self.worker.submit(job) {
            self.preview.pending = false;
            self.preview.kind = PreviewKind::Error;
            self.worker_error = Some(error.to_string());
            self.content_message = Some(format!("Package worker failed: {error}"));
            return Ok(());
        }
        self.preview.pending = true;
        self.content_message = Some(if self.compare.is_some() {
            format!("Comparing {display_name}…")
        } else {
            format!("Loading {display_name}…")
        });
        Ok(())
    }

    pub(super) fn is_directory(&self, path: &str) -> bool {
        self.index().is_directory(path)
            || self
                .compare
                .as_ref()
                .is_some_and(|compare| compare.package.index.is_directory(path))
    }
}
impl App {
    /// Discard the unsaved edits of the selected part. Destructive, so it asks
    /// for a second press like the quit guard does.
    pub fn request_revert(&mut self) {
        if self.is_saving() {
            self.status_message = Some("Save in progress".to_string());
            return;
        }
        let Some(selected) = self.tree.state.selected().last().cloned() else {
            self.status_message = Some("Select a package part to revert".to_string());
            return;
        };
        let display = selected.trim_start_matches('/').to_string();
        let on_screen =
            self.editor.previewed_path.as_deref() == Some(selected.as_str()) && self.editor.dirty;
        if !on_screen && !self.editor.edits.contains_key(&selected) {
            self.status_message = Some(format!("No unsaved edits in {display}"));
            return;
        }
        if self.pending_confirmation != Some(Confirmation::Revert(selected.clone())) {
            self.pending_confirmation = Some(Confirmation::Revert(selected));
            self.status_message = Some(format!(
                "Press {} again to discard unsaved edits in {display}",
                key_hint(Action::RevertPart)
            ));
            return;
        }
        self.pending_confirmation = None;
        if on_screen {
            if let Some(baseline) = self.editor.baseline.as_ref() {
                let spot = CursorSpot::of(&self.editor.state);
                self.editor.state = self.new_editor_state(baseline.clone());
                place_cursor(&mut self.editor.state, spot);
            }
            self.editor.dirty = false;
        } else {
            self.editor.edits.remove(&selected);
        }
        self.refresh_tree_labels();
        self.status_message = Some(format!("Reverted {display}"));
    }

    // -- Quit guard -------------------------------------------------------------

    /// Quitting with unsaved edits must be asked for twice.
    pub fn request_quit(&mut self) -> bool {
        // Dirty state is sticky during ordinary typing for performance; this
        // explicit decision reconciles it before deciding whether to warn.
        self.refresh_editor_dirty();
        if matches!(self.pending_confirmation, Some(Confirmation::Quit))
            || !self.has_unsaved_edits()
        {
            return true;
        }
        self.pending_confirmation = Some(Confirmation::Quit);
        self.status_message = Some(format!(
            "{} unsaved edit(s) — save with {}, quit again to discard",
            self.unsaved_edit_count(),
            key_hint(Action::SavePackage)
        ));
        false
    }

    /// Any other key cancels a pending confirmation, so it cannot be tripped by
    /// a later, unrelated keystroke.
    pub fn disarm_confirmation(&mut self) {
        if self.pending_confirmation.take().is_some() {
            self.status_message = None;
        }
    }

    /// Keep a pending confirmation only when this event can answer its exact
    /// destructive action in the current input context. Global bindings alone
    /// are insufficient: e.g. Emacs `q` is text, not a quit action.
    pub fn disarm_confirmation_unless(&mut self, confirms_quit: bool, confirms_revert: bool) {
        let confirms_pending = match self.pending_confirmation.as_ref() {
            Some(Confirmation::Quit) => confirms_quit,
            Some(Confirmation::Revert(_)) => confirms_revert,
            None => true,
        };
        if !confirms_pending {
            self.disarm_confirmation();
        }
    }

    pub fn open_help(&mut self) {
        self.show_help = true;
    }

    pub fn close_help(&mut self) {
        self.show_help = false;
    }

    pub fn toggle_unchanged_parts(&mut self) -> io::Result<()> {
        if self.compare.is_none() {
            self.status_message =
                Some("Unchanged filtering is only available in compare mode".into());
            return Ok(());
        }
        self.cancel_any_search();
        let selected = self.tree.state.selected().last().cloned();
        self.hide_unchanged_parts = !self.hide_unchanged_parts;
        self.install_tree();

        if let Some(selected) = selected.filter(|path| tree_contains(&self.tree.items, path)) {
            self.select_path(&selected);
        } else if let Some(path) = self.compare.as_ref().and_then(|compare| {
            compare
                .comparison
                .statuses
                .iter()
                .find(|(_, status)| **status != PartStatus::Unchanged)
                .map(|(path, _)| path.clone())
        }) {
            self.select_path(&path);
        } else if let Some(first) = self.tree.items.first() {
            self.tree.state.select(vec![first.identifier().clone()]);
        } else {
            self.tree.state.select(Vec::new());
        }
        self.load_selected_file_content_inner(false)
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
        // A relationship target must be a packaged part, so a directory or a path
        // that is not in the package explains itself instead of moving the
        // selection to an empty preview.
        if !crate::integrity::is_part(self.index(), &target) {
            self.status_message = Some(format!(
                "Relationship {} target is not a part: {target}",
                relationship.id
            ));
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
    /// XML preview can reference relationships, and only an `r:*` attribute value
    /// inside the enclosing start tag is treated as a reference.
    fn relationship_at_cursor(&self) -> Option<Relationship> {
        if self.preview.kind != PreviewKind::Xml {
            return None;
        }
        let previewed = self.editor.previewed_path.as_deref()?;
        let index = self.index();
        let relationships = index.outgoing.get(previewed)?;
        let (chars, column) = enclosing_start_tag(
            &self.editor.state.lines,
            self.editor.state.cursor.row,
            self.editor.state.cursor.col,
        )?;
        let is_name_char = |character: char| {
            matches!(
                character,
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | ':' | '.' | '-'
            )
        };

        for relationship_index in relationships {
            let relationship = &index.relationships[*relationship_index];
            if relationship.id.is_empty() {
                continue;
            }
            let id: Vec<char> = relationship.id.chars().collect();
            let width = id.len();
            for start in 0..chars.len().saturating_sub(width - 1) {
                if chars[start..start + width] != id[..] {
                    continue;
                }
                // The value must be a quoted attribute value. Both quote styles
                // XML allows reach the preview, which forwards the raw start tag
                // instead of normalizing it.
                let Some(quote_index) = start.checked_sub(1) else {
                    continue;
                };
                let quote = chars[quote_index];
                if quote != '"' && quote != '\'' {
                    continue;
                }
                if chars.get(start + width) != Some(&quote) {
                    continue;
                }
                // XML permits whitespace around `=`.
                let mut quote_start = quote_index;
                while quote_start > 0 && chars[quote_start - 1].is_ascii_whitespace() {
                    quote_start -= 1;
                }
                if quote_start == 0 || chars[quote_start - 1] != '=' {
                    continue;
                }
                // The token spans the `r:*` attribute name too, so the cursor on
                // either side of `=` follows the reference.
                let mut token_start = quote_start - 1;
                while token_start > 0 && chars[token_start - 1].is_ascii_whitespace() {
                    token_start -= 1;
                }
                while token_start > 0 && is_name_char(chars[token_start - 1]) {
                    token_start -= 1;
                }
                if chars[token_start] != 'r' || chars.get(token_start + 1) != Some(&':') {
                    continue;
                }
                if column >= token_start && column < start + width + 1 {
                    return Some(relationship.clone());
                }
            }
        }
        None
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
            .tree
            .state
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
    pub fn selection_status(&self) -> String {
        if self.save.active {
            let mut prompt = format!("Save to: {}_ | ", self.save.query);
            match self.save.confirm.as_deref() {
                Some(target) => {
                    prompt.push_str(&format!("{target} exists — Enter overwrite, Esc cancel"))
                }
                None => prompt.push_str("Enter save, Esc cancel"),
            }
            if let Some(first) = self.save.warnings.first() {
                let extra = self.save.warnings.len().saturating_sub(1);
                prompt.push_str(&format!(" | warning: {first}"));
                if extra > 0 {
                    prompt.push_str(&format!(" (+{extra} more)"));
                }
            }
            if let Some(message) = self.status_message.as_deref() {
                prompt.push_str(&format!(" | {message}"));
            }
            return prompt;
        }
        if self.export.active {
            return format!(
                "Extract to: {}_ | Enter save, Esc cancel",
                self.export.query
            );
        }
        let Some(selected) = self.tree.state.selected().last() else {
            let status = if self.content_search.active {
                format!(
                    "Content search: {}_ | {} matches | Enter finish, Esc cancel",
                    self.content_search.query,
                    self.content_search.matches.len()
                )
            } else if self.search.active {
                format!(
                    "Search: {}_ | {} matches | Enter select, Esc cancel",
                    self.search.query,
                    self.search.matches.len()
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
        if let Some(compare) = self.compare.as_ref()
            && let Some(diff) = compare.comparison.status_of(selected)
        {
            status.push_str(&format!(" | Diff: {}", diff.label()));
        }
        if self.content_search.active {
            status.push_str(&format!(
                " | Content search: {}_ | {} matches",
                self.content_search.query,
                self.content_search.matches.len()
            ));
        } else if !self.content_search.query.is_empty() {
            status.push_str(&format!(
                " | Content search: {} (n/N next, Esc clear)",
                self.content_search.query
            ));
        } else if self.search.active {
            status.push_str(&format!(" | Search: {}", self.search.query));
        } else if !self.search.query.is_empty() {
            status.push_str(&format!(
                " | Search: {} (n/N next, Esc clear)",
                self.search.query
            ));
        }
        self.with_status_message(status)
    }

    /// Export feedback has no other visible surface. Appending it last keeps it
    /// visible even when no tree item is selected.
    fn with_status_message(&self, mut status: String) -> String {
        if self.has_unsaved_edits() {
            status.push_str(&format!(
                " | {} unsaved edit(s), {} to save",
                self.unsaved_edit_count(),
                key_hint(Action::SavePackage)
            ));
        }
        if let Some(message) = self.status_message.as_deref() {
            status.push_str(" | ");
            status.push_str(message);
        }
        status
    }
}

/// Whether `character` is the `>` that ends a start tag, tracking whether a quoted
/// attribute value is in effect. XML only treats the quote character that opened a
/// value as its delimiter, so a literal `>`, `<`, or the other quote character
/// inside a value is ordinary text rather than a tag boundary.
fn ends_tag(quote: &mut Option<char>, character: char) -> bool {
    match *quote {
        Some(active) => {
            if character == active {
                *quote = None;
            }
            false
        }
        None => match character {
            '"' | '\'' => {
                *quote = Some(character);
                false
            }
            '>' => true,
            _ => false,
        },
    }
}

/// The start tag enclosing `(row, column)` exactly as the preview renders it,
/// plus the cursor's offset within it. A start tag can span several preview rows:
/// `pretty_print_xml` forwards the raw tag bytes, and XML allows a newline
/// between an attribute name, `=`, and its value.
fn enclosing_start_tag(lines: &Lines, row: usize, column: usize) -> Option<(Vec<char>, usize)> {
    let row_chars = |index: usize| lines.get(RowIndex::new(index));
    // A literal `<` inside a value has to be escaped, so the nearest one before the
    // cursor opens the tag the cursor may be in.
    let mut open = None;
    'open: for index in (row.saturating_sub(MAX_TAG_ROWS)..=row).rev() {
        let Some(chars) = row_chars(index) else {
            break;
        };
        let end = if index == row {
            (column + 1).min(chars.len())
        } else {
            chars.len()
        };
        for position in (0..end).rev() {
            if chars[position] == '<' {
                open = Some((index, position));
                break 'open;
            }
        }
    }
    let (open_row, open_column) = open?;

    // Collect the tag up to the `>` that closes it, remembering where the cursor
    // falls inside it. Quote tracking starts right after the `<`, where it is
    // unambiguous, and carries across rows because a value may span them.
    let mut quote = None;
    let mut tag = Vec::new();
    let mut cursor_offset = None;
    'tag: for index in open_row..=open_row.saturating_add(MAX_TAG_ROWS) {
        let Some(chars) = row_chars(index) else {
            break;
        };
        let from = if index == open_row { open_column } else { 0 };
        for (position, character) in chars.iter().enumerate().skip(from) {
            if index == row && position == column {
                cursor_offset = Some(tag.len());
            }
            if ends_tag(&mut quote, *character) {
                break 'tag;
            }
            tag.push(*character);
        }
    }

    // Comments, CDATA sections, and processing instructions hold no attributes.
    if tag.starts_with(&['<', '!']) || tag.starts_with(&['<', '?']) {
        return None;
    }
    // A cursor that the scan passed without recording sits after the tag that the
    // nearest `<` opened, so it is in element text rather than in an attribute.
    Some((tag, cursor_offset?))
}

#[cfg(test)]
mod tests;
