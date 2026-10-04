use std::{fs, io, path::Path};

use crossterm_keybind::{
    DisplayFormat, KeyBind, KeyBindTrait,
    event::{KeyCode, KeyEvent, KeyModifiers},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, KeyBind)]
pub enum Action {
    /// Quit the application from the tree or Vim normal mode.
    #[keybindings["q"]]
    Quit,
    /// Quit the application from the modeless Emacs editor.
    #[keybindings["Control+q"]]
    QuitEditor,
    /// Navigate to the previous package part.
    #[keybindings["Alt+Left"]]
    NavigateBack,
    /// Navigate to the next package part.
    #[keybindings["Alt+Right"]]
    NavigateForward,
    /// Toggle the help overlay.
    #[keybindings["?", "F1"]]
    ToggleHelp,
    /// Switch focus between the tree, metadata, and content panels.
    #[keybindings["Tab", "BackTab"]]
    ToggleFocus,
    /// Focus the package tree panel.
    #[keybindings["1"]]
    FocusTree,
    /// Focus the metadata panel.
    #[keybindings["2"]]
    FocusDetails,
    /// Focus the content panel.
    #[keybindings["3"]]
    FocusContent,
    /// Move down in the package tree.
    #[keybindings["j", "Down"]]
    MoveDown,
    /// Move up in the package tree.
    #[keybindings["k", "Up"]]
    MoveUp,
    /// Scroll down in the package tree.
    #[keybindings["Control+d"]]
    PageDown,
    /// Scroll up in the package tree.
    #[keybindings["Control+u"]]
    PageUp,
    /// Select the first visible tree item.
    #[keybindings["g"]]
    First,
    /// Select the last visible tree item.
    #[keybindings["G"]]
    Last,
    /// Toggle unchanged parts in package comparison mode.
    #[keybindings["u"]]
    ToggleUnchangedParts,
    /// Expand or collapse the selected tree node without exporting.
    #[keybindings["e"]]
    ToggleSelected,
    /// Open the selected part in the content preview.
    #[keybindings["Enter"]]
    OpenContent,
    /// Toggle the metadata panel.
    #[keybindings["d"]]
    ShowMetadata,
    /// Toggle the document-specific summary.
    #[keybindings["s"]]
    ShowSummary,
    /// Expand all tree nodes.
    #[keybindings["E", "Shift+E", "Shift+e"]]
    ExpandAll,
    /// Collapse all tree nodes.
    #[keybindings["C", "Shift+C", "Shift+c"]]
    CollapseAll,
    /// Start searching package paths.
    #[keybindings["/"]]
    StartSearch,
    /// Start searching part contents in the background.
    #[keybindings["Control+f"]]
    StartContentSearch,
    /// Extract the selected part to a file.
    #[keybindings["x"]]
    ExtractPart,
    /// Open the selected part in `$PAGER`/`$EDITOR` from a temporary file.
    #[keybindings["o"]]
    OpenPartExternally,
    /// Copy the pretty-printed part content to the clipboard (OSC 52).
    #[keybindings["y"]]
    CopyPartContent,
    /// Save the edited parts back to a package file (a new file by default).
    #[keybindings["Control+s", "F2"]]
    SavePackage,
    /// Hand the selected part to `$VISUAL`/`$EDITOR` and take the text back.
    #[keybindings["Control+e", "F4"]]
    EditPartExternally,
    /// Discard the unsaved edits of the selected part.
    #[keybindings["R", "Shift+R", "Shift+r"]]
    RevertPart,
    /// Select the next search result.
    #[keybindings["n"]]
    NextMatch,
    /// Select the previous search result.
    #[keybindings["N"]]
    PreviousMatch,
    /// Jump to the next part with an integrity issue.
    #[keybindings["i"]]
    NextIssue,
    /// Jump to the previous part with an integrity issue.
    #[keybindings["I"]]
    PreviousIssue,
    /// Follow the `r:id`/`r:embed` relationship reference under the editor cursor.
    #[keybindings["Control+g"]]
    FollowRelationship,
    /// Cancel a transient mode or overlay.
    #[keybindings["Esc"]]
    Cancel,
    /// Confirm a transient mode input.
    #[keybindings["Enter"]]
    Confirm,
    /// Delete the previous character in a transient input.
    #[keybindings["Backspace"]]
    Backspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorMode {
    Vim,
    Emacs,
}

impl EditorMode {
    pub fn from_config(value: Option<&str>) -> io::Result<Self> {
        match value.unwrap_or("vim").to_ascii_lowercase().as_str() {
            "vim" => Ok(Self::Vim),
            "emacs" => Ok(Self::Emacs),
            mode => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported editor mode: {mode}; expected 'vim' or 'emacs'"),
            )),
        }
    }
}

pub fn default_config_path() -> io::Result<std::path::PathBuf> {
    dirs::config_dir()
        .map(|path| path.join("oox").join("config.toml"))
        .ok_or_else(|| io::Error::other("could not determine the system config directory"))
}

pub fn resolve_config_path(explicit: Option<&Path>) -> io::Result<Option<std::path::PathBuf>> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("configuration file not found: {}", path.display()),
            ));
        }
        return Ok(Some(path.to_path_buf()));
    }

    let path = default_config_path()?;
    Ok(path.is_file().then_some(path))
}

pub fn load(path: Option<&Path>) -> io::Result<EditorMode> {
    let Some(path) = path else {
        Action::init_and_load(None::<crossterm_keybind::toml::Value>).map_err(io::Error::other)?;
        return Ok(EditorMode::Vim);
    };

    let text = fs::read_to_string(path)?;
    let mut config: crossterm_keybind::toml::Table =
        crossterm_keybind::toml::from_str(&text).map_err(io::Error::other)?;
    let editor_mode = config
        .get("editor")
        .and_then(crossterm_keybind::toml::Value::as_table)
        .and_then(|editor| editor.get("mode"))
        .and_then(crossterm_keybind::toml::Value::as_str);
    let mode = EditorMode::from_config(editor_mode)?;
    let keybindings = config.remove("keybindings").unwrap_or_else(|| {
        crossterm_keybind::toml::Value::Table(crossterm_keybind::toml::Table::new())
    });

    Action::init_and_load(Some(keybindings)).map_err(io::Error::other)?;
    Ok(mode)
}

/// A row in the help overlay: either an action with its configured keybindings
/// or a fixed informational line (e.g. mouse usage).
pub enum HelpRow {
    Binding(Action, &'static str),
    Text(&'static str),
}

/// Help content lives next to the action definitions so descriptions and
/// bindings stay in sync; the UI renders these sections verbatim.
pub fn help_sections() -> Vec<(&'static str, Vec<HelpRow>)> {
    vec![
        (
            "Panels",
            vec![
                HelpRow::Binding(Action::FocusTree, "Focus tree"),
                HelpRow::Binding(Action::FocusDetails, "Focus metadata"),
                HelpRow::Binding(Action::FocusContent, "Focus content"),
                HelpRow::Binding(Action::ToggleFocus, "Cycle panel focus"),
            ],
        ),
        (
            "Navigation",
            vec![
                HelpRow::Binding(Action::MoveDown, "Move down"),
                HelpRow::Binding(Action::MoveUp, "Move up"),
                HelpRow::Binding(Action::PageDown, "Scroll down"),
                HelpRow::Binding(Action::PageUp, "Scroll up"),
                HelpRow::Binding(Action::First, "First item"),
                HelpRow::Binding(Action::Last, "Last item"),
                HelpRow::Binding(Action::ToggleSelected, "Expand / collapse selected"),
                HelpRow::Binding(Action::OpenContent, "Open / preview selected"),
                HelpRow::Binding(Action::ShowMetadata, "Toggle metadata panel"),
                HelpRow::Binding(Action::ShowSummary, "Toggle document summary"),
                HelpRow::Text("Mouse click   Select/expand tree item"),
                HelpRow::Text("Mouse wheel   Scroll tree/metadata"),
                HelpRow::Text("Click link    Open related part"),
                HelpRow::Binding(Action::ExpandAll, "Expand all"),
                HelpRow::Binding(Action::CollapseAll, "Collapse all"),
                HelpRow::Binding(
                    Action::FollowRelationship,
                    "Open r:id/r:embed target at cursor",
                ),
            ],
        ),
        (
            "Search",
            vec![
                HelpRow::Binding(Action::StartSearch, "Filter package paths (live)"),
                HelpRow::Binding(
                    Action::StartContentSearch,
                    "Grep part contents (background)",
                ),
                HelpRow::Text("Enter         Keep filter, select first match"),
                HelpRow::Binding(Action::NextMatch, "Next match / content match"),
                HelpRow::Binding(Action::PreviousMatch, "Previous match"),
                HelpRow::Binding(Action::Cancel, "Cancel search / clear filter"),
            ],
        ),
        (
            "Comparison",
            vec![
                HelpRow::Binding(Action::ToggleUnchangedParts, "Toggle unchanged parts"),
                HelpRow::Text("+ Added (green)   ~ Changed (yellow)   - Removed (red)"),
            ],
        ),
        (
            "Integrity",
            vec![
                HelpRow::Binding(Action::NextIssue, "Next part with a package issue"),
                HelpRow::Binding(Action::PreviousIssue, "Previous part with a package issue"),
            ],
        ),
        (
            "Editing",
            vec![
                HelpRow::Text("The content pane is editable for XML, text and JSON parts"),
                HelpRow::Binding(Action::SavePackage, "Save edited parts to a new package"),
                HelpRow::Binding(
                    Action::EditPartExternally,
                    "Edit in $VISUAL / $EDITOR, then return",
                ),
                HelpRow::Binding(
                    Action::RevertPart,
                    "Discard unsaved edits of the part (press twice)",
                ),
                HelpRow::Text("●             Part with unsaved edits"),
            ],
        ),
        (
            "Export",
            vec![
                HelpRow::Binding(Action::ExtractPart, "Extract part to a file"),
                HelpRow::Binding(Action::OpenPartExternally, "Open in $PAGER / $EDITOR"),
                HelpRow::Binding(Action::CopyPartContent, "Copy pretty content to clipboard"),
            ],
        ),
        (
            "General",
            vec![
                HelpRow::Binding(Action::ToggleHelp, "Show this help"),
                HelpRow::Binding(Action::Quit, "Quit tree / Vim normal mode"),
                HelpRow::Binding(Action::QuitEditor, "Quit Emacs editor"),
                HelpRow::Binding(Action::NavigateBack, "Previous part"),
                HelpRow::Binding(Action::NavigateForward, "Next part"),
            ],
        ),
    ]
}

/// The configured key(s) for an action, for messages that tell the user which
/// key to press. Empty when bindings have not been initialized, so it is safe to
/// call from tests and from any code path that runs before startup.
pub fn key_hint(action: Action) -> String {
    action.key_bindings_display_with_format(&DisplayFormat::Abbreviation)
}

/// Whether edtui's Emacs keymap acts on `key` while the content pane has focus.
///
/// Emacs mode is modeless, so the editor owns typing (every plain or shifted
/// character, and Tab) plus the chords below; an app action bound to one of
/// them would steal it from the editor. Everything else, including a
/// user-configured chord such as `Alt+S`, is free for the app. edtui exposes no
/// way to query its keymap, so this mirrors `emacs_keybindings` in edtui
/// 0.11.7 and must be revisited when edtui is upgraded.
pub fn emacs_editor_owns(key: &KeyEvent) -> bool {
    let modifiers = key.modifiers;
    match key.code {
        KeyCode::Char(_) if modifiers == KeyModifiers::NONE || modifiers == KeyModifiers::SHIFT => {
            true
        }
        KeyCode::Char(character) if modifiers == KeyModifiers::CONTROL => matches!(
            character.to_ascii_lowercase(),
            'a' | 'b'
                | 'd'
                | 'e'
                | 'f'
                | 'g'
                | 'h'
                | 'j'
                | 'k'
                | 'n'
                | 'o'
                | 'p'
                | 'r'
                | 's'
                | 'u'
                | 'v'
                | 'y'
        ),
        KeyCode::Char(character) if modifiers == KeyModifiers::ALT => {
            matches!(character, '<' | '>' | 'b' | 'd' | 'e' | 'f' | 'u' | 'v')
        }
        KeyCode::Left | KeyCode::Right if modifiers == KeyModifiers::CONTROL => true,
        KeyCode::Backspace if modifiers == KeyModifiers::ALT => true,
        KeyCode::Tab
        | KeyCode::Enter
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown => modifiers == KeyModifiers::NONE,
        _ => false,
    }
}

pub fn generate(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("configuration file already exists: {}", path.display()),
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let content = format!(
        "# oox configuration\n# Key values are arrays of alternative single-key bindings.\n\n[editor]\nmode = \"vim\"\n\n[keybindings]\n{}",
        Action::toml_example()
    );
    fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::{Action, emacs_editor_owns};
    use crossterm_keybind::{
        KeyBindTrait,
        event::{KeyCode, KeyEvent, KeyModifiers},
    };

    /// The bindings are a process-wide table that is filled exactly once, as at
    /// startup; a second `init_and_load` would drop the default aliases.
    fn init_bindings() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            Action::init_and_load(None::<crossterm_keybind::toml::Value>).unwrap();
        });
    }

    #[test]
    fn shift_modified_uppercase_key_is_supported() {
        init_bindings();
        let event = KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT);
        let actions = Action::dispatch(&event);
        assert!(actions.contains(&Action::ExpandAll));
        assert!(!actions.contains(&Action::ToggleSelected));
        assert!(!actions.contains(&Action::ExtractPart));

        let expand = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);
        let expand_actions = Action::dispatch(&expand);
        assert!(expand_actions.contains(&Action::ToggleSelected));
        assert!(!expand_actions.contains(&Action::ExtractPart));

        let extract = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(Action::dispatch(&extract).contains(&Action::ExtractPart));
    }

    /// Ctrl+S reaches the editor in Vim mode, and F2/F4 are the fallbacks that
    /// edtui never consumes.
    #[test]
    fn save_and_external_edit_keys_are_dispatched() {
        init_bindings();
        let save = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(Action::dispatch(&save).contains(&Action::SavePackage));
        let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert!(Action::dispatch(&f2).contains(&Action::SavePackage));
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert!(Action::dispatch(&ctrl_e).contains(&Action::EditPartExternally));
        let f4 = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
        assert!(Action::dispatch(&f4).contains(&Action::EditPartExternally));
        // The tree keeps `e` and `s` for its own actions.
        let e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(Action::dispatch(&e).contains(&Action::ToggleSelected));
    }

    /// `R` is the revert key in both its raw and shift-modified spellings.
    #[test]
    fn revert_key_is_dispatched() {
        init_bindings();
        for event in [
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::SHIFT),
        ] {
            assert!(
                Action::dispatch(&event).contains(&Action::RevertPart),
                "{event:?} must revert"
            );
        }
    }

    /// In the Emacs content pane the editor keeps typing and its own chords,
    /// while any other binding stays usable for app actions such as saving.
    #[test]
    fn emacs_pane_leaves_only_editor_chords_to_the_editor() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        // Owned by the editor: Ctrl+S is its search, and typing is typing.
        assert!(emacs_editor_owns(&key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL
        )));
        assert!(emacs_editor_owns(&key(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        )));
        assert!(emacs_editor_owns(&key(
            KeyCode::Char('S'),
            KeyModifiers::SHIFT
        )));
        assert!(emacs_editor_owns(&key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(emacs_editor_owns(&key(
            KeyCode::Left,
            KeyModifiers::CONTROL
        )));
        assert!(emacs_editor_owns(&key(
            KeyCode::Backspace,
            KeyModifiers::ALT
        )));
        // Free for the app: the default F2 and chords edtui does not bind.
        assert!(!emacs_editor_owns(&key(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(!emacs_editor_owns(&key(
            KeyCode::Char('s'),
            KeyModifiers::ALT
        )));
        assert!(!emacs_editor_owns(&key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
        assert!(!emacs_editor_owns(&key(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
    }
}
