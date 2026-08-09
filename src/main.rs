use std::{error::Error, io, path::PathBuf};

use clap::Parser;

use app::{App, CurrentWidget};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
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

        if let Event::Key(key) = &event {
            if key.kind == event::KeyEventKind::Release {
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
