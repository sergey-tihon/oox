use super::edit::lines_to_text;
use super::*;

/// A save path is typed in the same prompt, so it shares the export bound.
const MAX_SAVE_PATH_CHARS: usize = 1024;

pub(crate) struct SaveFlow {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) confirm: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) request_id: u64,
    pub(crate) pending: bool,
    pub(crate) reloading: bool,
    pub(crate) reload_preview_request_id: Option<u64>,
    pub(crate) last_saved: Option<PathBuf>,
    pub(crate) pending_reselect: Option<Reselect>,
    pub(crate) pending_cursor: Option<Reselect>,
    pub(crate) post_load_message: Option<String>,
}

impl App {
    /// A save owns the buffers during both the write and the post-save reload;
    /// input resumes only after the reselected preview has finished installing.
    pub fn is_saving(&self) -> bool {
        self.save.pending || self.save.reloading
    }

    /// Every unsaved edit, ready for the save job: the parts set aside plus the
    /// buffer on screen when it differs from what was loaded.
    pub(super) fn collect_edits(&self) -> Vec<(String, Vec<u8>)> {
        let mut edits: BTreeMap<String, Vec<u8>> = self
            .editor
            .edits
            .iter()
            .map(|(path, edited)| {
                (
                    path.clone(),
                    lines_to_text(&edited.state.lines).into_bytes(),
                )
            })
            .collect();
        if let (Some(path), Some(baseline)) = (
            self.editor.previewed_path.as_ref(),
            self.editor.baseline.as_ref(),
        ) && self.editor.editable
            && self.editor.state.lines != *baseline
        {
            edits.insert(
                path.clone(),
                lines_to_text(&self.editor.state.lines).into_bytes(),
            );
        }
        edits.into_iter().collect()
    }

    /// Open the save prompt for the unsaved edits, pre-filled with the default
    /// target so the common case is Enter only.
    pub fn start_save(&mut self) {
        if self.is_saving() {
            self.status_message = Some("Save in progress".to_string());
            return;
        }
        if self.editor.external_edit_request.is_some() {
            self.status_message = Some("External edit is still loading".to_string());
            return;
        }
        if !self.is_package_loaded() {
            self.status_message = Some("Package is still loading".to_string());
            return;
        }
        if self.compare.is_some() {
            self.status_message = Some("Editing is unavailable in compare mode".to_string());
            return;
        }
        // Saving is explicit; reconcile once in case the user manually restored
        // the original text while the dirty flag stayed sticky.
        self.refresh_editor_dirty();
        if !self.has_unsaved_edits() {
            self.status_message = Some("No edits to save".to_string());
            return;
        }
        self.save.warnings = self.edit_warnings();
        self.save.query = self.default_save_target();
        self.save.confirm = None;
        self.save.active = true;
        self.status_message = None;
    }

    /// Advisory well-formedness verdicts for the parts about to be written. They
    /// are collected before the write so the prompt can show them while the user
    /// can still back out.
    pub(super) fn edit_warnings(&self) -> Vec<String> {
        let index = self.index();
        let mut edits = BTreeMap::<&str, String>::new();
        for (path, edited) in &self.editor.edits {
            if !index
                .parts
                .get(path)
                .is_some_and(|part| part.kind == PartKind::Xml)
            {
                continue;
            }
            edits.insert(path, lines_to_text(&edited.state.lines));
        }
        if let (Some(path), Some(baseline)) = (
            self.editor.previewed_path.as_deref(),
            self.editor.baseline.as_ref(),
        ) && self.editor.editable
            && self.editor.state.lines != *baseline
            && index
                .parts
                .get(path)
                .is_some_and(|part| part.kind == PartKind::Xml)
        {
            edits.insert(path, lines_to_text(&self.editor.state.lines));
        }

        edits
            .into_iter()
            .filter_map(|(path, text)| {
                crate::preview::xml_well_formed(&text).err().map(|problem| {
                    format!(
                        "{} is not well-formed XML ({problem})",
                        path.trim_start_matches('/')
                    )
                })
            })
            .collect()
    }

    pub fn save_input_char(&mut self, character: char) {
        if self.save.query.chars().count() >= MAX_SAVE_PATH_CHARS {
            return;
        }
        // Editing the path invalidates both the overwrite confirmation and any
        // message about the previous one.
        self.save.confirm = None;
        self.status_message = None;
        self.save.query.push(character);
    }

    pub fn save_backspace(&mut self) {
        self.save.confirm = None;
        self.status_message = None;
        self.save.query.pop();
    }

    pub fn cancel_save(&mut self) {
        self.save.active = false;
        self.save.query.clear();
        self.save.confirm = None;
        self.save.warnings.clear();
        self.status_message = None;
    }

    /// Any occupied target requires an explicit second confirmation, including
    /// a target this session saved earlier: another process may have changed it.
    pub fn confirm_save(&mut self) -> io::Result<()> {
        let target = self.save.query.trim().to_string();
        if target.is_empty() {
            self.status_message = Some("Save path is empty".to_string());
            return Ok(());
        }
        let target_path = PathBuf::from(&target);
        // A directory target can never be written, so say so in the prompt
        // instead of failing the write after the fact.
        if target_path.is_dir() {
            self.status_message = Some(format!("{target} is a directory"));
            return Ok(());
        }
        // `exists()` follows symlinks, so a dangling one would slip past the
        // confirmation and be replaced by the install. Any directory entry at
        // the path, symlink included, has to be asked about.
        let occupied = std::fs::symlink_metadata(&target_path).is_ok();
        // Authorization belongs to this exact prompt interaction; having saved
        // the path in the past does not protect against external modification.
        let overwrite = self.save.confirm.as_deref() == Some(&target);
        if occupied && !overwrite {
            // The prompt itself renders the confirmation, so no status message.
            self.save.confirm = Some(target.clone());
            return Ok(());
        }
        self.save.active = false;
        self.save.query.clear();
        self.save.confirm = None;
        self.save.warnings.clear();
        self.submit_save(target_path, overwrite)
    }

    pub(super) fn submit_save(&mut self, target: PathBuf, overwrite: bool) -> io::Result<()> {
        let Some(package) = self.package.as_ref() else {
            self.status_message = Some("Package is still loading".to_string());
            return Ok(());
        };
        let package_source = package.source.clone();
        let index = Arc::clone(&package.index);
        let edits = self.collect_edits();
        if edits.is_empty() {
            self.status_message = Some("No edits to save".to_string());
            return Ok(());
        }
        self.save.request_id = self.save.request_id.wrapping_add(1);
        let request_id = self.save.request_id;
        if let Err(error) = self.worker.submit(Job::SavePackage {
            request_id,
            package_path: package_source,
            target,
            overwrite,
            index,
            edits,
        }) {
            self.save.pending = false;
            self.status_message = Some(format!("Save failed: {error}"));
            return Ok(());
        }
        self.save.pending = true;
        self.status_message = Some("Saving…".to_string());
        Ok(())
    }

    /// Default target: the last path written this session, otherwise
    /// `<stem>.edited.<ext>` next to the file that was opened.
    pub(super) fn default_save_target(&self) -> String {
        if let Some(path) = self.save.last_saved.as_ref() {
            return path.to_string_lossy().to_string();
        }
        let source = Path::new(&self.file_path);
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("package");
        let extension = source
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("zip");
        let name = format!("{stem}.edited.{extension}");
        match source
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Some(directory) => directory.join(name).to_string_lossy().to_string(),
            None => name,
        }
    }

    /// Install a saved package: the edits are now on disk, so they are dropped
    /// and the package is re-indexed through the worker.
    pub(super) fn after_save(&mut self, path: PathBuf) {
        self.save.reloading = true;
        self.save.reload_preview_request_id = None;
        self.editor.edits.clear();
        self.editor.baseline = None;
        self.editor.editable = false;
        self.editor.dirty = false;
        self.save.last_saved = Some(path.clone());
        // The reload re-selects the saved part; carrying the cursor over keeps
        // the user on the line they were editing.
        let reselect = self.editor.previewed_path.take().map(|part| Reselect {
            path: part,
            spot: CursorSpot::of(&self.editor.state),
        });
        // Saving is refused in compare mode, so the comparison is not in play.
        self.file_path = path.to_string_lossy().to_string();
        self.open_request_id = self.open_request_id.wrapping_add(1);
        self.loading = true;
        self.save.pending_reselect = reselect;
        let request = Job::Open {
            request_id: self.open_request_id,
            path: path.clone(),
        };
        if let Err(error) = self.worker.submit(request) {
            self.loading = false;
            self.status_message = Some(format!(
                "Saved {}, but reloading it failed: {error}",
                path.display()
            ));
            self.save.reloading = false;
            self.save.reload_preview_request_id = None;
            self.save.pending_reselect = None;
            self.save.pending_cursor = None;
            return;
        }
        let message = format!("Saved {}", path.display());
        self.save.post_load_message = Some(message.clone());
        self.status_message = Some(message);
    }
}
