use edtui::{EditorStatusLine, EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    text::Text,
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation},
    Frame,
};
use ratatui_image::{Resize, StatefulImage};
use tui_tree_widget::Tree;

use crate::app::{App, CurrentWidget};

pub fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let accent_color = Color::LightGreen;
    let normal_style = Style::default().fg(Color::White);
    let active_style = Style::default()
        .fg(accent_color)
        .add_modifier(Modifier::BOLD);

    // Top section
    let title_block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default());

    let title = Paragraph::new(Text::styled(
        format!("File path: {}", app.file_path),
        Style::default().fg(accent_color),
    ))
    .block(title_block);

    f.render_widget(title, chunks[0]);

    // Middle section
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(chunks[1]);

    // Tree widget
    let tree_block = Block::bordered()
        .title("Document Inspector")
        .title_top(Line::from("[?] Help  [Tab]").right_aligned())
        .border_style(if app.current_widget == CurrentWidget::Tree {
            active_style
        } else {
            normal_style
        });

    match Tree::new(&app.tree_items) {
        Ok(tree_widget) => {
            let tree_widget = tree_widget
                .block(tree_block)
                .experimental_scrollbar(Some(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .begin_symbol(None)
                        .track_symbol(None)
                        .end_symbol(None),
                ))
                .highlight_style(
                    Style::new()
                        .fg(Color::Black)
                        .bg(accent_color)
                        .add_modifier(Modifier::BOLD),
                );
            f.render_stateful_widget(tree_widget, sections[0], &mut app.tree_state);
        }
        Err(error) => {
            let message = Paragraph::new(format!("Unable to render document tree: {error}"))
                .block(tree_block);
            f.render_widget(message, sections[0]);
        }
    }

    // Editor widget with XML syntax highlighting
    let editor_block = Block::default()
        .borders(Borders::ALL)
        .title("File content [Vim mode]")
        .title_top(Line::from("[Tab]").right_aligned())
        .border_style(if app.current_widget == CurrentWidget::TextArea {
            active_style
        } else {
            normal_style
        });

    if let Some(image_state) = app.image_state.as_mut() {
        let image_block = Block::default()
            .borders(Borders::ALL)
            .title("Image preview")
            .title_top(Line::from("[?] Help  [Tab]").right_aligned())
            .border_style(if app.current_widget == CurrentWidget::TextArea {
                active_style
            } else {
                normal_style
            });
        let image_area = image_block.inner(sections[1]);
        f.render_widget(image_block, sections[1]);

        let image = StatefulImage::default().resize(Resize::Fit(None));
        f.render_stateful_widget(image, image_area, image_state);
    } else if let Some(message) = app.status_message.as_deref() {
        let message = Paragraph::new(message)
            .block(editor_block)
            .style(Style::default().fg(Color::Yellow));
        f.render_widget(message, sections[1]);
    } else {
        let status_line = EditorStatusLine::default()
            .style_mode(Style::default().fg(Color::Black).bg(accent_color).bold())
            .style_search(Style::default().fg(Color::White))
            .style_line(Style::default());

        let theme = EditorTheme::default()
            .base(Style::default())
            .block(editor_block)
            .cursor_style(Style::default().bg(accent_color).fg(Color::Black))
            .selection_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .line_numbers_style(Style::default().fg(Color::DarkGray))
            .status_line(status_line);

        let syntax_highlighter = SyntaxHighlighter::new("dracula", "xml").ok();
        let editor_view = EditorView::new(&mut app.editor_state)
            .theme(theme)
            .line_numbers(LineNumbers::Absolute)
            .syntax_highlighter(syntax_highlighter);
        f.render_widget(editor_view, sections[1]);
    }

    let status = Paragraph::new(app.selection_status()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[2]);

    if app.show_help {
        let help_area = centered_rect(f.area(), 72, 78);
        f.render_widget(Clear, help_area);
        let help = Paragraph::new(Text::from(vec![
            Line::from("Navigation"),
            Line::from("  j / ↓        Move down"),
            Line::from("  k / ↑        Move up"),
            Line::from("  Ctrl-d       Scroll down"),
            Line::from("  Ctrl-u       Scroll up"),
            Line::from("  g / G         First / last item"),
            Line::from("  Enter         Expand or inspect part"),
            Line::from("  Tab           Switch tree/editor focus"),
            Line::from(""),
            Line::from("Search"),
            Line::from("  /             Search package paths"),
            Line::from("  Enter         Select first matching part"),
            Line::from("  n / N         Next / previous match"),
            Line::from("  Esc           Cancel search"),
            Line::from(""),
            Line::from("  ? / F1         Show this help"),
            Line::from("  q              Quit from tree or Vim Normal mode"),
        ]))
        .block(
            Block::bordered()
                .title("Help")
                .title_bottom(Line::from("[Esc] Close").right_aligned()),
        )
        .style(Style::default().fg(Color::White));
        f.render_widget(help, help_area);
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Percentage(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Percentage(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(vertical[1])[1]
}
