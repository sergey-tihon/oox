use std::{
    error::Error,
    fs::OpenOptions,
    io::{self, Write},
    path::PathBuf,
};

use clap::Parser;

use app::{App, CurrentWidget};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use edtui::{EditorEventHandler, EditorMode};
use ratatui::prelude::*;
use ratatui_image::picker::Picker;

mod app;
mod ui;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// OOXML document to inspect
    #[arg(value_name = "FILE", default_value = "data/sample.pptx")]
    file: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
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
    let mut editor_handler = EditorEventHandler::default();
    let result = run_app(&mut terminal, &mut app, &mut editor_handler);
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

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    app: &mut App,
    editor_handler: &mut EditorEventHandler,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::ui(f, app))?;

        let event = event::read()?;
        debug_log(format!("event={event:?}"));

        if let Event::Key(key) = &event {
            if key.kind == event::KeyEventKind::Release {
                continue;
            }

            debug_log(format!(
                "key={:?} modifiers={:?} focus={:?} editor_mode={:?} help={} search={}",
                key.code,
                key.modifiers,
                app.current_widget,
                app.editor_state.mode,
                app.show_help,
                app.search_active,
            ));

            if app.show_help {
                if matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?')) {
                    debug_log("closing help");
                    app.close_help();
                }
                continue;
            }

            if app.search_active && app.current_widget == CurrentWidget::Tree {
                match key.code {
                    KeyCode::Esc => {
                        debug_log("canceling search");
                        app.cancel_search();
                    }
                    KeyCode::Enter => {
                        debug_log(format!("finishing search query={:?}", app.search_query));
                        app.finish_search();
                    }
                    KeyCode::Backspace => app.search_backspace(),
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.search_input_char(character);
                    }
                    _ => {}
                }
                continue;
            }

            let can_show_help = match app.current_widget {
                CurrentWidget::Tree => true,
                CurrentWidget::TextArea => app.editor_state.mode == EditorMode::Normal,
            };
            // F1 is handled here in every editor mode. edtui does not support
            // function keys and would panic if it received one.
            if key.code == KeyCode::F(1) {
                debug_log("opening help via F1");
                app.open_help();
                continue;
            }
            if app.current_widget == CurrentWidget::TextArea && matches!(key.code, KeyCode::F(_)) {
                continue;
            }

            if key.code == KeyCode::Char('?') && can_show_help {
                debug_log("opening help via ?");
                app.open_help();
                continue;
            }

            // Global quit: 'q' only when editor is in Normal mode or when in Tree widget
            let can_quit = match app.current_widget {
                CurrentWidget::Tree => true,
                CurrentWidget::TextArea => app.editor_state.mode == EditorMode::Normal,
            };

            if key.code == KeyCode::Char('q') && can_quit {
                return Ok(());
            }

            // Widget switching with Tab (only in Normal mode for editor)
            if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
                let can_switch = match app.current_widget {
                    CurrentWidget::Tree => true,
                    CurrentWidget::TextArea => app.editor_state.mode == EditorMode::Normal,
                };

                if can_switch {
                    app.current_widget = match app.current_widget {
                        CurrentWidget::Tree => CurrentWidget::TextArea,
                        CurrentWidget::TextArea => CurrentWidget::Tree,
                    };
                    continue;
                }
            }
        }

        match app.current_widget {
            CurrentWidget::Tree => {
                if let Event::Key(key) = &event {
                    match key.code {
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.tree_state.key_down();
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.tree_state.key_up();
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.tree_state.scroll_down(10);
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.tree_state.scroll_up(10);
                        }
                        KeyCode::Char('g') => {
                            app.tree_state.select_first();
                        }
                        KeyCode::Char('G') => {
                            app.tree_state.select_last();
                        }
                        KeyCode::Char('/') => {
                            app.start_search();
                        }
                        KeyCode::Char('n') => {
                            app.next_search_match(false);
                        }
                        KeyCode::Char('N') => {
                            app.next_search_match(true);
                        }
                        KeyCode::Enter => {
                            app.tree_state.toggle_selected();
                            app.load_selected_file_content()?;
                        }
                        _ => {}
                    }
                }
            }
            CurrentWidget::TextArea => {
                editor_handler.on_event(event, &mut app.editor_state);
            }
        }
    }
}
