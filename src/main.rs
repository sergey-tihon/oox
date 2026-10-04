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
use edtui::{EditorEventHandler, EditorMode as EdtuiMode, EditorState};
use keybindings::Action;
use ratatui::prelude::*;
use ratatui_image::picker::Picker;
use worker::Worker;

mod app;
mod compare;
mod integrity;
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
    /// OOXML document to inspect, optionally with a second document to compare against
    #[arg(value_name = "FILE", num_args = 1..=2, required = true)]
    files: Vec<PathBuf>,
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
    let mut files = cli.files.into_iter();
    let file = files.next().ok_or("missing FILE argument")?;
    let compare = files.next();
    let mut app = App::new_loading(file.to_string_lossy().into_owned(), compare, picker, worker)?;

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
        PendingExport::OpenTemp(temp) => {
            // The snapshot deletes itself when `temp` drops at the end of this arm.
            let path = temp.path().to_path_buf();
            let result = open_external(terminal, &path);
            app.status_message = Some(match result {
                Ok(()) => format!("Returned from external viewer for {}", path.display()),
                Err(error) => format!("Could not open external viewer: {error}"),
            });
        }
    }
    Ok(())
}

/// `$PAGER` first, then `$EDITOR`, then `$VISUAL`: inspecting is the common case,
/// editing the read-only snapshot the rare one. The value is the raw command
/// string, including any options or quoted arguments.
fn external_command() -> Option<String> {
    ["PAGER", "EDITOR", "VISUAL"].iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

/// The editor for the round-trip edit. `$VISUAL` wins because it means "the
/// editor for interactive use"; `$PAGER` is deliberately not a candidate.
/// A GUI editor needs its wait flag (`code --wait`), as with git.
fn editor_command() -> Option<String> {
    ["VISUAL", "EDITOR"].iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

/// Hand the part's buffer to the external editor and take the text back. The
/// package is written only by the ordinary save, so quitting the editor without
/// saving leaves the document alone.
fn run_external_edit(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    app: &mut App,
) -> io::Result<()> {
    let Some(requested) = app.take_external_edit_request() else {
        return Ok(());
    };
    // The request is consumed first: a missing editor must not leave it pending,
    // which would make the event loop retry it on every iteration.
    let Some(editor) = editor_command() else {
        app.status_message = Some("No $VISUAL or $EDITOR is set".to_string());
        return Ok(());
    };
    // Failing to write the snapshot is recoverable; it must not end the session.
    let pending = match app.begin_external_edit() {
        Ok(pending) => pending,
        Err(error) => {
            app.status_message = Some(format!("Could not prepare the external edit: {error}"));
            return Ok(());
        }
    };
    let path = pending.temp.path().to_path_buf();
    // The snapshot deletes itself when `pending` drops at the end of this scope.
    // A stale `$VISUAL`/`$EDITOR` must not take the session down with it, so the
    // failure is reported the same way the external viewer reports it.
    match with_terminal_suspended(terminal, || run_external(&editor, &path)) {
        Ok(status) => match std::fs::read(&path) {
            Ok(bytes) => app.apply_external_edit(&pending.written, &bytes, status.success()),
            Err(error) => {
                app.status_message = Some(format!("Could not read the edited part: {error}"));
            }
        },
        Err(error) => {
            app.status_message = Some(format!("Could not run the editor: {error}"));
        }
    }
    debug_log(format!("external edit of {requested} finished"));
    Ok(())
}

/// Run the configured command against `path`, returning its exit status.
#[cfg(unix)]
fn run_external(command: &str, path: &Path) -> io::Result<std::process::ExitStatus> {
    // A shell keeps quoted arguments such as `nvim -c 'set readonly'` intact,
    // while the part path is passed as a real positional argument ($1) so a path
    // containing spaces or metacharacters cannot be reinterpreted by the shell.
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
}

/// Fallback for platforms without `sh`: split on whitespace and pass the path
/// as a separate argument.
#[cfg(not(unix))]
fn run_external(command: &str, path: &Path) -> io::Result<std::process::ExitStatus> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty external command"))?;
    std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
}

fn open_external(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    path: &Path,
) -> io::Result<()> {
    let command = external_command().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no $PAGER, $EDITOR, or $VISUAL is set",
        )
    })?;
    with_terminal_suspended(terminal, || {
        let status = run_external(&command, path)?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{command} exited with {status}")))
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

/// Decide whether an editor key can change buffer contents and whether it can
/// restore the baseline (undo/redo). Pure cursor motions and search keys skip
/// the full buffer comparison; once dirty, ordinary edits remain sticky until
/// an undo/redo or explicit save/navigation reconciliation.
fn editor_key_dirty_policy(
    code: KeyCode,
    modifiers: KeyModifiers,
    editor_mode: keybindings::EditorMode,
    state_mode: EdtuiMode,
    vim_operator_pending: bool,
) -> (bool, bool, bool) {
    let character = match code {
        KeyCode::Char(character) => Some(character),
        _ => None,
    };
    let unmodified = modifiers == KeyModifiers::NONE;
    let plain_character = character.is_some() && (unmodified || modifiers == KeyModifiers::SHIFT);

    if editor_mode == keybindings::EditorMode::Emacs {
        let control = modifiers == KeyModifiers::CONTROL;
        let alt = modifiers == KeyModifiers::ALT;
        let restores_baseline = control && matches!(character, Some('u' | 'r' | 'U' | 'R'));
        let mutates = plain_character
            || (unmodified
                && matches!(
                    code,
                    KeyCode::Tab | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete
                ))
            || (control && matches!(character, Some('d' | 'h' | 'j' | 'k' | 'o' | 'y')))
            || (alt && matches!(character, Some('d' | 'u')))
            || (alt && code == KeyCode::Backspace);
        return (mutates || restores_baseline, restores_baseline, false);
    }

    if state_mode == EdtuiMode::Search {
        return (false, false, false);
    }
    if state_mode == EdtuiMode::Insert {
        let mutates = plain_character
            || (unmodified
                && matches!(
                    code,
                    KeyCode::Tab | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete
                ))
            || modifiers.contains(KeyModifiers::CONTROL)
                && !matches!(code, KeyCode::Left | KeyCode::Right);
        return (mutates, false, false);
    }

    let restores_baseline =
        (state_mode == EdtuiMode::Normal && unmodified && character == Some('u'))
            || (modifiers == KeyModifiers::CONTROL && character == Some('r'));
    if restores_baseline {
        return (true, true, false);
    }

    if vim_operator_pending {
        let continues_operator = matches!(
            character,
            Some('i' | 'a' | 'f' | 'F' | 't' | 'T' | 'u' | 'U' | '~')
        );
        return (true, false, continues_operator);
    }

    let starts_operator =
        unmodified && matches!(character, Some('d' | 'c' | 'y' | 'g' | '>' | '<' | '!'));
    if starts_operator {
        return (false, false, true);
    }

    let mutates = unmodified
        && matches!(
            character,
            Some(
                'x' | 'X'
                    | 'r'
                    | 'R'
                    | 's'
                    | 'S'
                    | 'D'
                    | 'C'
                    | 'p'
                    | 'P'
                    | 'J'
                    | '~'
                    | 'o'
                    | 'O'
                    | '.'
            )
        )
        || code == KeyCode::Delete;
    (mutates, false, false)
}

/// Whether this key can actually quit from the current editor context. The
/// global `Quit` binding is only a quit action in tree/details and Vim Normal;
/// modeless Emacs only quits through `QuitEditor`.
fn can_quit_from(
    current_widget: CurrentWidget,
    editor_mode: keybindings::EditorMode,
    editor_state_mode: EdtuiMode,
    actions: &[Action],
) -> bool {
    match current_widget {
        CurrentWidget::Tree | CurrentWidget::Details => actions.contains(&Action::Quit),
        CurrentWidget::TextArea => match editor_mode {
            keybindings::EditorMode::Vim => {
                actions.contains(&Action::Quit) && editor_state_mode == EdtuiMode::Normal
            }
            keybindings::EditorMode::Emacs => actions.contains(&Action::QuitEditor),
        },
    }
}

/// edtui's Emacs handler registers its modeless keymap in Insert mode. Fresh
/// previews create a Normal-mode state, so put it in Insert before dispatching
/// any Emacs input; subsequent non-Normal modes (e.g. search) are left alone.
fn ensure_emacs_insert_mode(state: &mut EditorState, editor_mode: keybindings::EditorMode) {
    if editor_mode == keybindings::EditorMode::Emacs && state.mode == EdtuiMode::Normal {
        state.mode = EdtuiMode::Insert;
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
    let mut vim_operator_pending = false;
    loop {
        if app.poll_worker() {
            redraw = true;
        }
        ensure_emacs_insert_mode(&mut app.editor_state, editor_mode);
        // The requested part has to be on screen before the editor can take over.
        if !app.save_active && !app.is_saving() && app.external_edit_ready() {
            redraw = true;
            run_external_edit(terminal, app)?;
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

        // A save in flight owns `previewed_path` and the buffers: navigation
        // would move the edit into `edits` and make the reload reselect the
        // wrong part, and quitting would detach a worker that is still writing.
        // Poll and redraw above stay live so the result is still delivered.
        if app.is_saving() {
            continue;
        }

        if let Event::Mouse(mouse) = &event {
            // Any-motion tracking fires while the pointer merely moves; that is
            // not the user answering a confirmation, so only real gestures disarm.
            if !matches!(mouse.kind, MouseEventKind::Moved) {
                app.disarm_confirmation();
            }
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
                    // edtui only maps a click to the cursor when it lands in the
                    // text area; a border/gutter/status-line click leaves the
                    // cursor untouched and must not follow a stale reference.
                    let follows_reference =
                        matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                            && ui::content_text_area_contains(
                                terminal_area,
                                app,
                                mouse.column,
                                mouse.row,
                            );
                    editor_handler.on_event(event, &mut app.editor_state);
                    // Mouse handling changes cursor/viewport/selection, not text.
                    vim_operator_pending = false;
                    if follows_reference {
                        app.follow_relationship_at_cursor()?;
                    }
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
            // Only preserve a confirmation for an action that can actually
            // answer it here. In Emacs, plain `q` is text; only `QuitEditor`
            // can confirm quit. Revert is valid only from Tree/Details.
            let modal_active = app.show_help
                || app.search_active
                || app.content_search_active
                || app.export_active
                || app.save_active;
            let can_quit = !modal_active
                && can_quit_from(
                    app.current_widget,
                    editor_mode,
                    app.editor_state.mode,
                    actions,
                );
            let can_revert = !modal_active
                && app.is_package_loaded()
                && matches!(
                    app.current_widget,
                    CurrentWidget::Tree | CurrentWidget::Details
                )
                && actions.contains(&Action::RevertPart);
            app.disarm_confirmation_unless(can_quit, can_revert);
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

            if app.save_active {
                if actions.contains(&Action::Cancel) {
                    app.cancel_save();
                } else if actions.contains(&Action::Confirm) {
                    debug_log(format!("confirming save path={:?}", app.save_query));
                    app.confirm_save()?;
                } else if actions.contains(&Action::Backspace) {
                    app.save_backspace();
                } else if let KeyCode::Char(character) = key.code {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.save_input_char(character);
                    }
                }
                continue;
            }

            // In Emacs mode the editor owns typing and its own chords (Ctrl+S is
            // search there), so only those are left to it; any other configured
            // save binding, such as F2 or a custom Alt+S, still saves. Vim mode
            // has no conflicting save chord.
            let can_save = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => match editor_mode {
                    keybindings::EditorMode::Vim => true,
                    keybindings::EditorMode::Emacs => !keybindings::emacs_editor_owns(key),
                },
            };
            if actions.contains(&Action::SavePackage) && can_save && app.is_package_loaded() {
                debug_log("opening the save prompt");
                app.start_save();
                continue;
            }

            // Plain `O` is not usable: uppercase keys are already taken by the
            // tree bindings and Vim's `O` in the editor. F4 is the editor key
            // everywhere, and Ctrl+E outside the editor.
            let can_edit_externally = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => matches!(key.code, KeyCode::F(_)),
            };
            if actions.contains(&Action::EditPartExternally)
                && can_edit_externally
                && app.is_package_loaded()
            {
                debug_log("requesting external edit");
                app.request_external_edit()?;
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
            if app.current_widget == CurrentWidget::TextArea
                && actions.contains(&Action::FollowRelationship)
            {
                if !app.follow_relationship_at_cursor()? {
                    app.status_message =
                        Some("No relationship reference under the cursor".to_string());
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

            if can_quit {
                if app.request_quit() {
                    return Ok(());
                }
                continue;
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
                    } else if actions.contains(&Action::ToggleUnchangedParts) {
                        app.toggle_unchanged_parts()?;
                    } else if actions.contains(&Action::ExpandAll) {
                        app.expand_all();
                    } else if actions.contains(&Action::ToggleSelected) {
                        app.tree_state.toggle_selected();
                    } else if actions.contains(&Action::OpenContent) {
                        app.tree_state.toggle_selected();
                        app.load_selected_file_content()?;
                    } else if actions.contains(&Action::ShowMetadata) {
                        app.toggle_details();
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
                    } else if actions.contains(&Action::NextIssue) {
                        app.next_integrity_issue(false);
                    } else if actions.contains(&Action::PreviousIssue) {
                        app.next_integrity_issue(true);
                    } else if actions.contains(&Action::ExtractPart) {
                        app.start_extract();
                    } else if actions.contains(&Action::OpenPartExternally) {
                        app.open_selected_externally()?;
                    } else if actions.contains(&Action::CopyPartContent) {
                        app.copy_selected_content()?;
                    } else if actions.contains(&Action::RevertPart) {
                        app.request_revert();
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
                    } else if actions.contains(&Action::NextIssue) {
                        app.next_integrity_issue(false);
                    } else if actions.contains(&Action::PreviousIssue) {
                        app.next_integrity_issue(true);
                    } else if actions.contains(&Action::ExtractPart) {
                        app.start_extract();
                    } else if actions.contains(&Action::OpenPartExternally) {
                        app.open_selected_externally()?;
                    } else if actions.contains(&Action::CopyPartContent) {
                        app.copy_selected_content()?;
                    } else if actions.contains(&Action::RevertPart) {
                        app.request_revert();
                    }
                }
            }
            CurrentWidget::TextArea => {
                let (may_change_content, may_restore_baseline) = match &event {
                    Event::Key(key) => {
                        let (may_change, restores, pending) = editor_key_dirty_policy(
                            key.code,
                            key.modifiers,
                            editor_mode,
                            app.editor_state.mode,
                            vim_operator_pending,
                        );
                        vim_operator_pending = pending;
                        (may_change, restores)
                    }
                    Event::Paste(_) => {
                        vim_operator_pending = false;
                        (true, false)
                    }
                    _ => {
                        vim_operator_pending = false;
                        (false, false)
                    }
                };
                editor_handler.on_event(event, &mut app.editor_state);
                app.refresh_editor_dirty_after_input(may_change_content, may_restore_baseline);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CLIPBOARD_BYTES, base64_encode, can_quit_from, editor_key_dirty_policy,
        ensure_emacs_insert_mode, osc52_sequence,
    };
    use crate::{
        app::CurrentWidget,
        keybindings::{Action, EditorMode},
    };
    use crossterm_keybind::event::{KeyCode, KeyModifiers};
    use edtui::EditorMode as EdtuiMode;

    #[test]
    fn emacs_editor_starts_in_edtui_insert_mode() {
        let mut state = edtui::EditorState::default();
        ensure_emacs_insert_mode(&mut state, EditorMode::Emacs);
        assert_eq!(state.mode, EdtuiMode::Insert);

        // Search has its own active state and must not be reset by redraws.
        state.mode = EdtuiMode::Search;
        ensure_emacs_insert_mode(&mut state, EditorMode::Emacs);
        assert_eq!(state.mode, EdtuiMode::Search);

        let mut state = edtui::EditorState::default();
        ensure_emacs_insert_mode(&mut state, EditorMode::Vim);
        assert_eq!(state.mode, EdtuiMode::Normal);
    }
    #[test]
    fn confirmation_quit_action_is_context_valid() {
        let quit = [Action::Quit];
        let quit_editor = [Action::QuitEditor];
        assert!(!can_quit_from(
            CurrentWidget::TextArea,
            EditorMode::Emacs,
            EdtuiMode::Insert,
            &quit,
        ));
        assert!(can_quit_from(
            CurrentWidget::TextArea,
            EditorMode::Emacs,
            EdtuiMode::Insert,
            &quit_editor,
        ));
        assert!(!can_quit_from(
            CurrentWidget::TextArea,
            EditorMode::Vim,
            EdtuiMode::Insert,
            &quit,
        ));
        assert!(can_quit_from(
            CurrentWidget::Tree,
            EditorMode::Emacs,
            EdtuiMode::Insert,
            &quit,
        ));
    }

    #[test]
    fn editor_dirty_policy_skips_motion_and_reconciles_undo() {
        assert_eq!(
            editor_key_dirty_policy(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                EditorMode::Vim,
                EdtuiMode::Normal,
                false,
            ),
            (false, false, false)
        );
        assert_eq!(
            editor_key_dirty_policy(
                KeyCode::Char('d'),
                KeyModifiers::NONE,
                EditorMode::Vim,
                EdtuiMode::Normal,
                false,
            ),
            (false, false, true)
        );
        assert_eq!(
            editor_key_dirty_policy(
                KeyCode::Char('w'),
                KeyModifiers::NONE,
                EditorMode::Vim,
                EdtuiMode::Normal,
                true,
            ),
            (true, false, false)
        );
        assert_eq!(
            editor_key_dirty_policy(
                KeyCode::Char('u'),
                KeyModifiers::NONE,
                EditorMode::Vim,
                EdtuiMode::Normal,
                false,
            ),
            (true, true, false)
        );
        assert_eq!(
            editor_key_dirty_policy(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL,
                EditorMode::Emacs,
                EdtuiMode::Insert,
                false,
            ),
            (true, true, false)
        );
    }

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

    /// The configured command may carry options and quoted arguments; a shell
    /// must pass them through unchanged while the part path stays a single
    /// argument.
    #[cfg(unix)]
    #[test]
    fn external_command_preserves_quoted_arguments() {
        let output =
            std::env::temp_dir().join(format!("oox-test-external-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&output);

        let status = super::run_external(
            &format!("printf '<%s>' 'a b' > {}", output.display()),
            std::path::Path::new("/tmp/part.xml"),
        )
        .expect("shell should run");
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&output).expect("output should be written"),
            "<a b></tmp/part.xml>"
        );

        let _ = std::fs::remove_file(&output);
    }
}
