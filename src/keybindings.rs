use std::{fs, io, path::Path};

use crossterm_keybind::{KeyBind, KeyBindTrait};

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
    /// Open the selected part in the content preview.
    #[keybindings["Enter"]]
    OpenContent,
    /// Toggle the metadata panel.
    #[keybindings["d"]]
    ShowMetadata,
    /// Expand all tree nodes.
    #[keybindings["E", "Shift+E", "Shift+e"]]
    ExpandAll,
    /// Collapse all tree nodes.
    #[keybindings["C", "Shift+C", "Shift+c"]]
    CollapseAll,
    /// Start searching package paths.
    #[keybindings["/"]]
    StartSearch,
    /// Select the next search result.
    #[keybindings["n"]]
    NextMatch,
    /// Select the previous search result.
    #[keybindings["N"]]
    PreviousMatch,
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
    use super::Action;
    use crossterm_keybind::{
        event::{KeyCode, KeyEvent, KeyModifiers},
        KeyBindTrait,
    };

    #[test]
    fn shift_modified_uppercase_key_is_supported() {
        Action::init_and_load(None::<crossterm_keybind::toml::Value>).unwrap();
        let event = KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT);
        assert!(Action::dispatch(&event).contains(&Action::ExpandAll));
    }
}
