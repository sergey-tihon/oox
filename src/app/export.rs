use super::edit::lines_to_text;
use super::*;

const MAX_EXPORT_PATH_CHARS: usize = 1024;

pub(crate) struct ExportFlow {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) request_id: u64,
    pub(crate) pending: bool,
    pub(crate) ready: Option<PendingExport>,
}

impl App {
    pub(super) fn selected_exportable_part(&self) -> Option<PartInfo> {
        let selected = self.tree.state.selected().last()?;
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
        if let Some(selected) = self.tree.state.selected().last() {
            self.export.query = selected.rsplit('/').next().unwrap_or(selected).to_string();
        }
        self.export.active = true;
    }

    pub fn export_input_char(&mut self, character: char) {
        if self.export.query.chars().count() >= MAX_EXPORT_PATH_CHARS {
            return;
        }
        self.export.query.push(character);
    }

    pub fn export_backspace(&mut self) {
        self.export.query.pop();
    }

    pub fn cancel_extract(&mut self) {
        self.export.active = false;
        self.export.query.clear();
    }

    pub fn confirm_extract(&mut self) -> io::Result<()> {
        self.export.active = false;
        let destination = self.export.query.trim().to_string();
        self.export.query.clear();
        if destination.is_empty() {
            self.status_message = Some("Extraction path is empty".to_string());
            return Ok(());
        }
        self.submit_export(ExportMode::SaveTo(PathBuf::from(destination)))
    }

    /// Write the selected part to a temporary file and hand it to `$PAGER`/`$EDITOR`.
    pub fn open_selected_externally(&mut self) -> io::Result<()> {
        if let Some(text) = self.unsaved_text_of_selected() {
            let Some(part) = self.selected_exportable_part() else {
                self.status_message = Some("Select a package part to export".to_string());
                return Ok(());
            };
            match crate::worker::write_temp_file(&part.archive_name, text.as_bytes()) {
                Ok(temp) => {
                    self.export.ready = Some(PendingExport::OpenTemp(temp));
                }
                Err(error) => {
                    self.status_message = Some(format!("Export failed: {error}"));
                }
            }
            return Ok(());
        }
        self.submit_export(ExportMode::OpenTemp)
    }

    /// Copy the pretty-printed preview text of the selected part as OSC 52.
    pub fn copy_selected_content(&mut self) -> io::Result<()> {
        if let Some(text) = self.unsaved_text_of_selected() {
            self.export.ready = Some(PendingExport::Clipboard(text));
            return Ok(());
        }
        self.submit_export(ExportMode::Clipboard)
    }

    /// The unsaved buffer of the selected part, when there is one. Copy and the
    /// external viewer must see what the user is looking at, not the bytes still
    /// on disk.
    pub(super) fn unsaved_text_of_selected(&self) -> Option<String> {
        let selected = self.tree.state.selected().last()?;
        if self.editor.previewed_path.as_deref() == Some(selected.as_str()) && self.editor.dirty {
            return Some(lines_to_text(&self.editor.state.lines));
        }
        self.editor
            .edits
            .get(selected)
            .map(|edited| lines_to_text(&edited.state.lines))
    }

    /// Hand a finished export to the event loop, which owns the terminal.
    pub fn take_pending_export(&mut self) -> Option<PendingExport> {
        self.export.ready.take()
    }

    pub(super) fn submit_export(&mut self, mode: ExportMode) -> io::Result<()> {
        if self.export.pending {
            self.status_message = Some("Export already in progress".to_string());
            return Ok(());
        }
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
        self.export.request_id = self.export.request_id.wrapping_add(1);
        let request_id = self.export.request_id;
        if let Err(error) = self.worker.submit(Job::ExportPart {
            request_id,
            package_path: package_source,
            part: Box::new(part),
            index,
            mode,
        }) {
            self.export.pending = false;
            self.status_message = Some(format!("Export failed: {error}"));
            return Ok(());
        }
        self.export.pending = true;
        self.status_message = Some("Preparing export…".to_string());
        Ok(())
    }
}
