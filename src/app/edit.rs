use super::*;
pub(crate) struct EditedPart {
    pub(crate) state: EditorState,
    pub(crate) baseline: Lines,
    pub(crate) kind: PreviewKind,
}

pub(crate) struct EditorSession {
    pub(crate) state: EditorState,
    pub(crate) clipboard: edtui::clipboard::Clipboard,
    pub(crate) baseline: Option<Lines>,
    pub(crate) editable: bool,
    pub(crate) dirty: bool,
    pub(crate) edits: BTreeMap<String, EditedPart>,
    pub(crate) previewed_path: Option<String>,
    pub(crate) external_edit_request: Option<String>,
}

pub(crate) struct ExternalEdit {
    pub(crate) temp: TempPart,
    pub(crate) written: String,
}

impl App {
    // -- Editable content pane -------------------------------------------------

    /// Whether the pane's text can be written back into the package.
    pub fn editor_editable(&self) -> bool {
        self.editor.editable
    }

    /// Whether the pane shows an unsaved change.
    pub fn editor_is_dirty(&self) -> bool {
        self.editor.dirty
    }

    /// Parts with unsaved edits, counting the buffer on screen.
    pub fn unsaved_edit_count(&self) -> usize {
        self.editor.edits.len() + usize::from(self.editor.dirty)
    }

    pub fn has_unsaved_edits(&self) -> bool {
        self.unsaved_edit_count() > 0
    }

    /// Re-diff the buffer against its baseline. Used for explicit reconciliation
    /// (save, quit, navigation, undo/redo), not cursor-only editor events.
    pub fn refresh_editor_dirty(&mut self) {
        let dirty = match (&self.editor.previewed_path, &self.editor.baseline) {
            (Some(_), Some(baseline)) => {
                self.editor.editable && self.editor.state.lines != *baseline
            }
            _ => false,
        };
        if dirty != self.editor.dirty {
            self.editor.dirty = dirty;
            self.refresh_tree_labels();
        }
    }

    /// Editor events only need a full baseline comparison when their input can
    /// change content. Once dirty, ordinary edits stay dirty without rescanning;
    /// undo/redo reconciles against the baseline so undoing back to the original
    /// clears the marker.
    pub fn refresh_editor_dirty_after_input(
        &mut self,
        may_change_content: bool,
        may_restore_baseline: bool,
    ) {
        if !may_change_content && !may_restore_baseline {
            return;
        }
        if self.editor.dirty && !may_restore_baseline {
            return;
        }
        self.refresh_editor_dirty();
    }

    /// Set the buffer of the part on screen aside before the pane is replaced,
    /// so an unsaved edit is not lost when the selection moves on.
    pub(super) fn stash_current_edit(&mut self) {
        let Some(path) = self.editor.previewed_path.take() else {
            return;
        };
        let Some(baseline) = self.editor.baseline.take() else {
            return;
        };
        let was_dirty = std::mem::replace(&mut self.editor.dirty, false);
        if !self.editor.editable || self.editor.state.lines == baseline {
            if was_dirty || self.editor.edits.remove(&path).is_some() {
                self.refresh_tree_labels();
            }
            return;
        }
        let blank = self.new_editor_state(Lines::default());
        let state = std::mem::replace(&mut self.editor.state, blank);
        self.editor.edits.insert(
            path,
            EditedPart {
                state,
                baseline,
                kind: self.preview.kind,
            },
        );
        // The part is now marked from `edits` rather than from the buffer.
        self.refresh_tree_labels();
    }

    /// Bring back the buffer of a part that was edited before, instead of
    /// reloading (and discarding) the version on disk.
    pub(super) fn restore_edit(&mut self, path: &str) -> bool {
        let Some(edited) = self.editor.edits.remove(path) else {
            return false;
        };
        self.editor.state = edited.state;
        self.editor.baseline = Some(edited.baseline);
        self.editor.editable = true;
        self.preview.kind = edited.kind;
        self.editor.previewed_path = Some(path.to_string());
        self.content_message = None;
        self.editor.dirty = true;
        true
    }
    // -- External editor --------------------------------------------------------

    /// Ask for the selected part to be edited in `$VISUAL`/`$EDITOR`. The event
    /// loop launches the editor once the part's buffer is on screen.
    pub fn request_external_edit(&mut self) -> io::Result<()> {
        if self.save.active {
            self.status_message = Some("Save prompt is active".to_string());
            return Ok(());
        }
        if self.is_saving() {
            self.status_message = Some("Save in progress".to_string());
            return Ok(());
        }
        if !self.is_package_loaded() {
            self.status_message = Some("Package is still loading".to_string());
            return Ok(());
        }
        if self.compare.is_some() {
            self.status_message = Some("Editing is unavailable in compare mode".to_string());
            return Ok(());
        }
        let Some(selected) = self.tree.state.selected().last().cloned() else {
            self.status_message = Some("Select a package part to edit".to_string());
            return Ok(());
        };
        // A directory has no bytes to hand over; without this the request would
        // stay pending forever, because no preview can ever match it.
        if self.selected_exportable_part().is_none() {
            self.status_message = Some("Select a package part to edit".to_string());
            return Ok(());
        }
        self.editor.external_edit_request = Some(selected.clone());
        if self.editor.previewed_path.as_deref() != Some(selected.as_str()) {
            self.load_selected_file_content()?;
        }
        Ok(())
    }

    /// Whether the requested part is on screen and ready to be handed over.
    pub fn external_edit_ready(&self) -> bool {
        if self.save.active || self.is_saving() {
            return false;
        }
        match self.editor.external_edit_request.as_deref() {
            Some(path) => self.editor.previewed_path.as_deref() == Some(path),
            None => false,
        }
    }

    pub fn take_external_edit_request(&mut self) -> Option<String> {
        let path = self.editor.external_edit_request.take()?;
        if !self.editor.editable {
            self.status_message = Some("This part is read-only".to_string());
            return None;
        }
        Some(path)
    }

    /// Forget a pending request for a part that turned out not to be editable.
    pub(super) fn forget_external_edit(&mut self, path: &str) {
        if self.editor.external_edit_request.as_deref() == Some(path) {
            self.editor.external_edit_request = None;
            self.status_message = Some("This part cannot be edited".to_string());
        }
    }

    /// Write the current buffer to a temporary file named after the part, so the
    /// editor picks the right syntax mode.
    pub fn begin_external_edit(&mut self) -> io::Result<ExternalEdit> {
        let text = lines_to_text(&self.editor.state.lines);
        let archive_name = self
            .editor
            .previewed_path
            .as_deref()
            .and_then(|path| self.index().parts.get(path))
            .map(|part| part.archive_name.clone())
            .unwrap_or_else(|| "part.xml".to_string());
        let temp = crate::worker::write_temp_file(&archive_name, text.as_bytes())?;
        Ok(ExternalEdit {
            temp,
            written: text,
        })
    }

    /// Take the edited text back into the buffer. An external edit is just
    /// another way to change the same unsaved buffer, so it still needs an
    /// explicit save to reach the package.
    pub fn apply_external_edit(&mut self, written: &str, bytes: &[u8], success: bool) {
        if !success {
            self.status_message = Some("Editor exited with an error; changes ignored".to_string());
            return;
        }
        if self.editor.previewed_path.is_none() || !self.editor.editable {
            return;
        }
        match pick_up_external(written, bytes) {
            Ok(None) => self.status_message = Some("No changes from the external editor".into()),
            Ok(Some(text)) => {
                let spot = CursorSpot::of(&self.editor.state);
                self.editor.state = self.new_editor_state(Lines::from(text.as_str()));
                place_cursor(&mut self.editor.state, spot);
                self.refresh_editor_dirty();
                self.status_message = Some(format!(
                    "{} edited externally — save to keep it",
                    self.editor
                        .previewed_path
                        .as_deref()
                        .unwrap_or_default()
                        .trim_start_matches('/')
                ));
            }
            Err(error) => {
                self.status_message = Some(format!("External edit ignored: {error}"));
            }
        }
    }
}

/// trailing newline, so a round trip through the editor changes nothing.
pub(super) fn lines_to_text(lines: &Lines) -> String {
    lines.to_string()
}

/// Decide what an external editor round trip means: `Ok(None)` when nothing
/// changed, `Ok(Some(text))` for a new buffer, `Err` when the file cannot be
/// used. Changes are detected by content, so a `:q` or a re-save is a no-op.
pub(super) fn pick_up_external(written: &str, bytes: &[u8]) -> io::Result<Option<String>> {
    // The same bound the part was read under, so a text part that could be
    // previewed can always be taken back. A tighter limit here would hand the
    // editor a buffer and then silently refuse the unchanged file it returns.
    if bytes.len() as u64 > crate::package::MAX_ENTRY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the edited file is too large",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not valid UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Compare on normalized endings so a Windows round trip is not an edit.
    let normalized = text.replace("\r\n", "\n");
    let written_normalized = written.replace("\r\n", "\n");
    // An editor is allowed to add the final newline the buffer was missing. The
    // tolerance is one direction only: when the buffer already ended in `\n`, a
    // second one is a real, added blank line and must be kept.
    let trailing_tolerance = !written_normalized.ends_with('\n')
        && normalized
            .strip_suffix('\n')
            .is_some_and(|trimmed| trimmed == written_normalized);
    if normalized == written_normalized || trailing_tolerance {
        return Ok(None);
    }
    Ok(Some(normalized))
}
