use edtui::{EditorStatusLine, EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    text::Text,
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation},
    Frame,
};
use ratatui_image::{Resize, StatefulImage};
use tui_tree_widget::Tree;

use crate::app::{App, CurrentWidget};

pub fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
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
        .title_top(Line::from("[Tab]").right_aligned())
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
            .title_top(Line::from("[Tab]").right_aligned())
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
}
