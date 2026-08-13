use std::{
    error::Error,
    fs::OpenOptions,
    io::{self, Write},
    path::PathBuf,
};

use clap::Parser;

use app::{App, CurrentWidget};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use crossterm_keybind::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use crossterm_keybind::KeyBindTrait;
use edtui::{EditorEventHandler, EditorMode as EdtuiMode};
use keybindings::Action;
use ratatui::prelude::*;
use ratatui_image::picker::Picker;

mod app;
mod keybindings;
mod ui;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// OOXML document to inspect
    #[arg(value_name = "FILE", default_value = "data/sample.pptx")]
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
    let mut app = App::from_file(cli.file.to_string_lossy().into_owned(), picker)?;

    enable_raw_mode()?;
    let mut stderr = io::stderr();
    if let Err(error) = execute!(stderr, EnterAlternateScreen, EnableMouseCapture) {
        disable_raw_mode()?;
        return Err(error.into());
    }
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let mut editor_handler = match editor_mode {
        keybindings::EditorMode::Vim => EditorEventHandler::vim_mode(),
        keybindings::EditorMode::Emacs => EditorEventHandler::emacs_mode(),
    };
    let result = run_app(&mut terminal, &mut app, &mut editor_handler, editor_mode);
    let restore_result = restore_terminal(&mut terminal);

    result?;
    restore_result?;
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

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )?;
    terminal.show_cursor()?;
    Ok(())
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
    loop {
        terminal.draw(|f| ui::ui(f, app))?;

        let event = event::read()?;
        debug_log(format!("event={event:?}"));

        if let Event::Mouse(mouse) = &event {
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

            if ui::tree_area_contains(terminal_area, app, mouse.column, mouse.row) {
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

        if let Event::Key(key) = &event {
            if key.kind == event::KeyEventKind::Release {
                continue;
            }

            let actions = Action::dispatch(key);
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

            if actions.contains(&Action::NavigateBack) {
                app.navigate_back()?;
                continue;
            }
            if actions.contains(&Action::NavigateForward) {
                app.navigate_forward()?;
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

            if matches!(
                app.current_widget,
                CurrentWidget::Tree | CurrentWidget::Details
            ) && actions.contains(&Action::ShowSummary)
            {
                app.toggle_summary()?;
                continue;
            }

            let can_show_help = match app.current_widget {
                CurrentWidget::Tree | CurrentWidget::Details => true,
                CurrentWidget::TextArea => app.editor_state.mode == EdtuiMode::Normal,
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

        match app.current_widget {
            CurrentWidget::Tree => {
                if let Event::Key(key) = &event {
                    let actions = Action::dispatch(key);
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
                    } else if actions.contains(&Action::NextMatch) {
                        app.next_search_match(false);
                    } else if actions.contains(&Action::PreviousMatch) {
                        app.next_search_match(true);
                    }
                }
            }
            CurrentWidget::Details => {
                if let Event::Key(key) = &event {
                    let actions = Action::dispatch(key);
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
                    }
                }
            }
            CurrentWidget::TextArea => {
                editor_handler.on_event(event, &mut app.editor_state);
            }
        }
    }
}
