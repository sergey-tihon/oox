use std::{
    error::Error,
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Parser;

use app::{App, CurrentWidget, PendingExport};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use crossterm_keybind::KeyBindTrait;
use crossterm_keybind::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use edtui::{EditorEventHandler, EditorMode as EdtuiMode};
use keybindings::Action;
use ratatui::prelude::*;
use ratatui_image::picker::Picker;
use worker::Worker;

mod app;
mod keybindings;
mod layout;
mod package;
mod preview;
mod summary;
mod ui;
mod worker;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// OOXML document to inspect
    #[arg(value_name = "FILE")]
    file: PathBuf,
    /// Keybinding and editor configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Generate a documented default configuration file and exit
    #[arg(long)]
    generate_config: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if cli.generate_config {
        let path = match cli.config.as_deref() {
            Some(path) => path.to_path_buf(),
            None => keybindings::default_config_path()?,
        };
        keybindings::generate(&path)?;
        println!("Generated configuration at {}", path.display());
        return Ok(());
    }

    let config_path = keybindings::resolve_config_path(cli.config.as_deref())?;
    let editor_mode = keybindings::load(config_path.as_deref())?;
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let worker = Worker::start()?;
    let mut app = App::new_loading(cli.file.to_string_lossy().into_owned(), picker, worker)?;

    let stderr = io::stderr();
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    enable_raw_mode()?;
    if let Err(error) = execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    ) {
        let _ = disable_raw_mode();
        let _ = execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        return Err(error.into());
    }
    let mut editor_handler = match editor_mode {
        keybindings::EditorMode::Vim => EditorEventHandler::vim_mode(),
        keybindings::EditorMode::Emacs => EditorEventHandler::emacs_mode(),
    };
    let mut terminal_guard = TerminalGuard { terminal };
    run_app(
        &mut terminal_guard.terminal,
        &mut app,
        &mut editor_handler,
        editor_mode,
    )?;
    Ok(())
}

const DEBUG_LOG_PATH: &str = "/tmp/oox-debug.log";

fn debug_log(message: impl std::fmt::Display) {
    if std::env::var_os("OOX_DEBUG").is_none() {
        return;
    }

    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(DEBUG_LOG_PATH)
    {
        let _ = writeln!(file, "[oox-debug] {message}");
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stderr>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        );
        let _ = self.terminal.show_cursor();
    }
}

/// Finish background export work. Only the event loop has the terminal, so this
/// is where external commands run and OSC 52 bytes reach the backend.
fn apply_export(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    app: &mut App,
    export: PendingExport,
) -> io::Result<()> {
    match export {
        PendingExport::Extracted(path) => {
            app.status_message = Some(format!("Extracted part to {}", path.display()));
        }
        PendingExport::Clipboard(text) => match osc52_sequence(&text) {
            Ok(sequence) => {
                terminal.backend_mut().write_all(sequence.as_bytes())?;
                io::Write::flush(terminal.backend_mut())?;
                app.status_message = Some(format!(
                    "Copied {} bytes to the clipboard (OSC 52)",
                    text.len()
                ));
            }
            Err(error) => {
                app.status_message = Some(format!("Clipboard copy failed: {error}"));
            }
        },
        PendingExport::OpenTemp(path) => {
            let result = open_external(terminal, &path);
            // The temp copy is a read-only snapshot; drop it on return.
            let _ = std::fs::remove_file(&path);
            app.status_message = Some(match result {
                Ok(()) => format!(
                    "Returned from {} for {}",
                    external_command_label(),
                    path.display()
                ),
                Err(error) => format!("Could not open external viewer: {error}"),
            });
        }
    }
    Ok(())
}

/// `$PAGER` first, then `$EDITOR`, then `$VISUAL`: inspecting is the common case,
/// editing the read-only snapshot the rare one.
fn external_command() -> Option<(String, Vec<String>)> {
    let value = ["PAGER", "EDITOR", "VISUAL"].iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })?;
    let mut parts = value.split_whitespace();
    let program = parts.next()?.to_string();
    Some((program, parts.map(str::to_string).collect()))
}

fn external_command_label() -> String {
    external_command()
        .map(|(program, _)| program)
        .unwrap_or_default()
}

fn open_external(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    path: &Path,
) -> io::Result<()> {
    let (program, args) = external_command().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no $PAGER, $EDITOR, or $VISUAL is set",
        )
    })?;
    with_terminal_suspended(terminal, || {
        let status = std::process::Command::new(&program)
            .args(&args)
            .arg(path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{program} exited with {status}")))
        }
    })
}

/// Hand the terminal to a child process, then always take it back, so a failed
/// or interrupted viewer cannot leave `oox` in a broken state.
fn with_terminal_suspended<T>(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    run: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    let result = run();

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;
    result
}

/// Terminals truncate oversized OSC 52 payloads, so refuse rather than silently
/// copying a partial part.
const MAX_CLIPBOARD_BYTES: usize = 100_000;

fn osc52_sequence(text: &str) -> io::Result<String> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("content exceeds the {MAX_CLIPBOARD_BYTES} byte clipboard limit"),
        ));
    }
    Ok(format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes())))
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let third = u32::from(*chunk.get(2).unwrap_or(&0));
        let packed =
            (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | third;
        encoded.push(ALPHABET[(packed >> 18) as usize & 0x3f] as char);
        encoded.push(ALPHABET[(packed >> 12) as usize & 0x3f] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(packed >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[packed as usize & 0x3f] as char
        } else {
            '='
        });
    }
    encoded
}

fn next_focus(current: CurrentWidget, details_visible: bool, backwards: bool) -> CurrentWidget {
    match (current, details_visible, backwards) {
        (CurrentWidget::Tree, true, false) => CurrentWidget::Details,
        (CurrentWidget::Tree, false, false) => CurrentWidget::TextArea,
        (CurrentWidget::Details, _, false) => CurrentWidget::TextArea,
        (CurrentWidget::TextArea, true, false) => CurrentWidget::Tree,
        (CurrentWidget::TextArea, false, false) => CurrentWidget::Tree,
        (CurrentWidget::Tree, true, true) => CurrentWidget::TextArea,
        (CurrentWidget::Tree, false, true) => CurrentWidget::TextArea,
        (CurrentWidget::Details, _, true) => CurrentWidget::Tree,
        (CurrentWidget::TextArea, true, true) => CurrentWidget::Details,
        (CurrentWidget::TextArea, false, true) => CurrentWidget::Tree,
    }
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    app: &mut App,
    editor_handler: &mut EditorEventHandler,
    editor_mode: keybindings::EditorMode,
) -> io::Result<()> {
    // Redraw-on-change: render once, then again only when the worker reports new
    // state or a terminal event arrives. Idle polling no longer repaints at 20 fps.
    let mut redraw = true;
    loop {
        if app.poll_worker() {
            redraw = true;
        }
        if let Some(export) = app.take_pending_export() {
            redraw = true;
            apply_export(terminal, app, export)?;
        }
        if redraw {
            terminal.draw(|f| ui::ui(f, app))?;
            redraw = false;
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        redraw = true;
        debug_log(format!("event={event:?}"));

        if let Event::Mouse(mouse) = &event {
            if app.show_help {
                continue;
            }
            let terminal_area = terminal.size()?.into();
            if let Some(line) = ui::summary_line_at(terminal_area, app, mouse.column, mouse.row) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => app.scroll_summary(-3),
                    MouseEventKind::ScrollDown => app.scroll_summary(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.activate_summary_link(line.0, line.1)?;
                    }
                    _ => {}
                }
                continue;
            }

            if let Some(line) = ui::metadata_line_at(terminal_area, app, mouse.column, mouse.row) {
                app.current_widget = CurrentWidget::Details;
                match mouse.kind {
                    MouseEventKind::ScrollUp => app.scroll_details(-3),
                    MouseEventKind::ScrollDown => app.scroll_details(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.activate_detail_link(line.0, line.1)?;
                    }
                    _ => {}
                }
                continue;
            }

            if ui::content_area_contains(terminal_area, app, mouse.column, mouse.row) {
                if app.is_package_loaded() {
                    app.current_widget = CurrentWidget::TextArea;
                    editor_handler.on_event(event, &mut app.editor_state);
                }
                continue;
            }

            if ui::tree_area_contains(terminal_area, app, mouse.column, mouse.row) {
                if !app.is_package_loaded() {
                    continue;
                }
                app.current_widget = CurrentWidget::Tree;
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        app.tree_state.scroll_up(3);
                    }
                    MouseEventKind::ScrollDown => {
                        app.tree_state.scroll_down(3);
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        let position = Position {
                            x: mouse.column,
                            y: mouse.row,
                        };
                        if app.tree_state.click_at(position) {
                            app.load_selected_file_content()?;
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }

        let dispatched_actions = match &event {
            Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                Some(Action::dispatch(key))
            }
            _ => None,
        };
        if let Event::Key(key) = &event {
            if key.kind == event::KeyEventKind::Release {
                continue;
            }

            let actions = dispatched_actions.as_deref().unwrap_or(&[]);
            debug_log(format!(
                "key={:?} modifiers={:?} actions={actions:?} focus={:?} editor_mode={:?} help={} search={}",
                key.code,
                key.modifiers,
                app.current_widget,
                app.editor_state.mode,
                app.show_help,
                app.search_active,
            ));

            if app.show_help {
                if actions.contains(&Action::Cancel) || actions.contains(&Action::ToggleHelp) {
                    debug_log("closing help");
                    app.close_help();
                }
                continue;
            }

            if app.content_search_active && app.current_widget == CurrentWidget::Tree {
                if actions.contains(&Action::Cancel) {
                    debug_log("canceling content search");
                    app.cancel_content_search();
                } else if actions.contains(&Action::Confirm) {
                    debug_log(format!(
                        "finishing content search query={:?}",
                        app.content_search_query
                    ));
                    app.finish_content_search();
                } else if actions.contains(&Action::Backspace) {
                    app.content_search_backspace();
                } else {
                    match key.code {
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            app.content_search_input_char(character);
                        }
                        _ => {}
                    }
                }
                continue;
            }

            if app.search_active && app.current_widget == CurrentWidget::Tree {
                if actions.contains(&Action::Cancel) {
                    debug_log("canceling search");
                    app.cancel_search();
                } else if actions.contains(&Action::Confirm) {
                    debug_log(format!("finishing search query={:?}", app.search_query));
                    app.finish_search();
                } else if actions.contains(&Action::Backspace) {
                    app.search_backspace();
                } else {
                    match key.code {
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            app.search_input_char(character);
                        }
                        _ => {}
                    }
                }
                continue;
            }

            if app.export_active {
                if actions.contains(&Action::Cancel) {
                    debug_log("canceling extract");
                    app.cancel_extract();
                } else if actions.contains(&Action::Confirm) {
                    debug_log(format!("confirming extract path={:?}", app.export_query));
                    app.confirm_extract()?;
                } else if actions.contains(&Action::Backspace) {
                    app.export_backspace();
                } else if let KeyCode::Char(character) = key.code {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.export_input_char(character);
                    }
                }
                continue;
            }

            if actions.contains(&Action::NavigateBack) {
                if app.is_package_loaded() {
                    app.navigate_back()?;
                }
                continue;
            }
            if actions.contains(&Action::NavigateForward) {
                if app.is_package_loaded() {
                    app.navigate_forward()?;
                }
                continue;
            }

            let can_focus_panel = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => {
                    editor_mode == keybindings::EditorMode::Vim
                        && app.editor_state.mode == EdtuiMode::Normal
                }
            };
            if can_focus_panel && actions.contains(&Action::FocusTree) {
                app.current_widget = CurrentWidget::Tree;
                continue;
            }
            if can_focus_panel && actions.contains(&Action::FocusDetails) {
                app.details_visible = true;
                app.current_widget = CurrentWidget::Details;
                continue;
            }
            if can_focus_panel && actions.contains(&Action::FocusContent) {
                app.current_widget = CurrentWidget::TextArea;
                continue;
            }

            if app.is_package_loaded()
                && matches!(
                    app.current_widget,
                    CurrentWidget::Tree | CurrentWidget::Details
                )
                && actions.contains(&Action::ShowSummary)
            {
                app.toggle_summary()?;
                continue;
            }

            let can_show_help = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => match editor_mode {
                    keybindings::EditorMode::Vim => app.editor_state.mode == EdtuiMode::Normal,
                    // Emacs mode is modeless: character keys insert text, so only
                    // function keys (e.g. F1) can open help from the editor.
                    keybindings::EditorMode::Emacs => matches!(key.code, KeyCode::F(_)),
                },
            };
            if actions.contains(&Action::ToggleHelp) && can_show_help {
                debug_log("opening help");
                app.open_help();
                continue;
            }
            // edtui does not support unhandled function keys.
            if app.current_widget == CurrentWidget::TextArea && matches!(key.code, KeyCode::F(_)) {
                continue;
            }

            let can_quit = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => actions.contains(&Action::Quit),
                CurrentWidget::TextArea => match editor_mode {
                    keybindings::EditorMode::Vim => {
                        actions.contains(&Action::Quit)
                            && app.editor_state.mode == EdtuiMode::Normal
                    }
                    keybindings::EditorMode::Emacs => actions.contains(&Action::QuitEditor),
                },
            };
            if can_quit {
                return Ok(());
            }

            let can_switch = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => {
                    editor_mode == keybindings::EditorMode::Emacs
                        || app.editor_state.mode == EdtuiMode::Normal
                }
            };
            if actions.contains(&Action::ToggleFocus) && can_switch {
                let backwards = key.code == KeyCode::BackTab;
                app.current_widget = next_focus(app.current_widget, app.details_visible, backwards);
                continue;
            }
        }

        if !app.is_package_loaded() {
            continue;
        }

        match app.current_widget {
            CurrentWidget::Tree => {
                if let Event::Key(_key) = &event {
                    let actions = dispatched_actions.as_deref().unwrap_or(&[]);
                    if actions.contains(&Action::MoveDown) {
                        app.tree_state.key_down();
                    } else if actions.contains(&Action::MoveUp) {
                        app.tree_state.key_up();
                    } else if actions.contains(&Action::PageDown) {
                        app.tree_state.scroll_down(10);
                    } else if actions.contains(&Action::PageUp) {
                        app.tree_state.scroll_up(10);
                    } else if actions.contains(&Action::First) {
                        app.tree_state.select_first();
                    } else if actions.contains(&Action::Last) {
                        app.tree_state.select_last();
                    } else if actions.contains(&Action::OpenContent) {
                        app.tree_state.toggle_selected();
                        app.load_selected_file_content()?;
                    } else if actions.contains(&Action::ShowMetadata) {
                        app.toggle_details();
                    } else if actions.contains(&Action::ExpandAll) {
                        app.expand_all();
                    } else if actions.contains(&Action::CollapseAll) {
                        app.collapse_all();
                    } else if actions.contains(&Action::StartSearch) {
                        app.start_search();
                    } else if actions.contains(&Action::StartContentSearch) {
                        app.start_content_search();
                    } else if actions.contains(&Action::NextMatch) {
                        if app.has_content_search_query() {
                            app.next_content_search_match(false);
                        } else {
                            app.next_search_match(false);
                        }
                    } else if actions.contains(&Action::PreviousMatch) {
                        if app.has_content_search_query() {
                            app.next_content_search_match(true);
                        } else {
                            app.next_search_match(true);
                        }
                    } else if actions.contains(&Action::ExtractPart) {
                        app.start_extract();
                    } else if actions.contains(&Action::OpenPartExternally) {
                        app.open_selected_externally()?;
                    } else if actions.contains(&Action::CopyPartContent) {
                        app.copy_selected_content()?;
                    } else if actions.contains(&Action::Cancel)
                        && (!app.search_query.is_empty() || app.has_content_search_query())
                    {
                        // Esc with an applied (but inactive) search clears the filter
                        // and restores the pre-search tree state.
                        app.cancel_any_search();
                    }
                }
            }
            CurrentWidget::Details => {
                if let Event::Key(_key) = &event {
                    let actions = dispatched_actions.as_deref().unwrap_or(&[]);
                    if actions.contains(&Action::MoveDown) {
                        app.move_details_cursor(false);
                    } else if actions.contains(&Action::MoveUp) {
                        app.move_details_cursor(true);
                    } else if actions.contains(&Action::PageDown) {
                        app.scroll_details(10);
                    } else if actions.contains(&Action::PageUp) {
                        app.scroll_details(-10);
                    } else if actions.contains(&Action::OpenContent)
                        || actions.contains(&Action::Confirm)
                    {
                        app.activate_current_detail_link()?;
                    } else if actions.contains(&Action::ShowMetadata) {
                        app.toggle_details();
                        if !app.details_visible {
                            app.current_widget = CurrentWidget::Tree;
                        }
                    } else if actions.contains(&Action::ExtractPart) {
                        app.start_extract();
                    } else if actions.contains(&Action::OpenPartExternally) {
                        app.open_selected_externally()?;
                    } else if actions.contains(&Action::CopyPartContent) {
                        app.copy_selected_content()?;
                    }
                }
            }
            CurrentWidget::TextArea => {
                editor_handler.on_event(event, &mut app.editor_state);
            }
        }
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::{MAX_CLIPBOARD_BYTES, base64_encode, osc52_sequence};

    #[test]
    fn base64_matches_rfc4648_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn oversized_clipboard_content_is_refused() {
        assert!(osc52_sequence(&"a".repeat(MAX_CLIPBOARD_BYTES + 1)).is_err());
        assert_eq!(osc52_sequence("hi").unwrap(), "\x1b]52;c;aGk=\x07");
    }
}
