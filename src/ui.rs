use crossterm_keybind::{DisplayFormat, KeyBindTrait};
use edtui::{EditorStatusLine, EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation},
    Frame,
};
use ratatui_image::{Resize, StatefulImage};
use tui_tree_widget::Tree;

use crate::{
    app::{App, CurrentWidget, PreviewKind},
    keybindings::Action,
};

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
    let left_sections = if app.details_visible {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(sections[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)])
            .split(sections[0])
    };

    // Tree widget
    let tree_block = Block::bordered()
        .title("[1] Document Inspector")
        .title_top(Line::from("[?] Help").right_aligned())
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
            f.render_stateful_widget(tree_widget, left_sections[0], &mut app.tree_state);
        }
        Err(error) => {
            let message = Paragraph::new(format!("Unable to render document tree: {error}"))
                .block(tree_block);
            f.render_widget(message, left_sections[0]);
        }
    }

    if app.details_visible {
        let details = app.details_view();
        let details_block = Block::bordered()
            .title("[2] Metadata")
            .title_top(Line::from("[d] Hide").right_aligned())
            .border_style(if app.current_widget == CurrentWidget::Details {
                active_style
            } else {
                normal_style
            });
        let details_inner = details_block.inner(left_sections[1]);
        let max_scroll = details
            .text
            .lines()
            .count()
            .saturating_sub(details_inner.height as usize) as u16;
        app.details_scroll = app.details_scroll.min(max_scroll);
        if details.links.is_empty() {
            app.details_cursor = 0;
        } else {
            app.details_cursor = app.details_cursor.min(details.links.len() - 1);
        }
        let selected_link_line = details.links.get(app.details_cursor).map(|link| link.line);
        let lines = details
            .text
            .lines()
            .enumerate()
            .map(|(line, text)| {
                if let Some(link) = details.links.iter().find(|link| link.line == line) {
                    let link_style = if Some(line) == selected_link_line {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::UNDERLINED)
                    };
                    let prefix = text.chars().take(link.start).collect::<String>();
                    let target = text
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>();
                    let suffix = text.chars().skip(link.end).collect::<String>();
                    Line::from(vec![
                        Span::raw(prefix),
                        Span::styled(target, link_style),
                        Span::raw(suffix),
                    ])
                } else {
                    Line::from(text.to_string())
                }
            })
            .collect::<Vec<_>>();
        let details = Paragraph::new(Text::from(lines))
            .block(details_block)
            .scroll((app.details_scroll, 0));
        f.render_widget(details, left_sections[1]);
    }

    // Content preview with XML syntax highlighting
    let editor_block = Block::default()
        .borders(Borders::ALL)
        .title(format!("[3] {}", content_title(app.preview_kind)))
        .title_top(Line::from("[Tab]").right_aligned())
        .border_style(if app.current_widget == CurrentWidget::TextArea {
            active_style
        } else {
            normal_style
        });

    if let Some(image_state) = app.image_state.as_mut() {
        let image_block = Block::default()
            .borders(Borders::ALL)
            .title(format!("[3] {}", content_title(app.preview_kind)))
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
        let color = if app.preview_kind == PreviewKind::Info {
            Color::White
        } else {
            Color::Yellow
        };
        let message = Paragraph::new(message)
            .block(editor_block)
            .style(Style::default().fg(color));
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

        let syntax_highlighter = match app.preview_kind {
            PreviewKind::Xml => SyntaxHighlighter::new("dracula", "xml").ok(),
            PreviewKind::Json => SyntaxHighlighter::new("dracula", "json").ok(),
            _ => None,
        };
        let line_numbers = if app.preview_kind == PreviewKind::Hex {
            LineNumbers::None
        } else {
            LineNumbers::Absolute
        };
        let editor_view = EditorView::new(&mut app.editor_state)
            .theme(theme)
            .line_numbers(line_numbers)
            .syntax_highlighter(syntax_highlighter);
        f.render_widget(editor_view, sections[1]);
    }

    let status = Paragraph::new(app.selection_status()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[2]);

    if app.show_help {
        let help_area = centered_rect(f.area(), 72, 78);
        f.render_widget(Clear, help_area);
        let help = Paragraph::new(Text::from(vec![
            Line::from("Panels"),
            help_line(&Action::FocusTree, "Focus tree"),
            help_line(&Action::FocusDetails, "Focus metadata"),
            help_line(&Action::FocusContent, "Focus content"),
            help_line(&Action::ToggleFocus, "Cycle panel focus"),
            Line::from(""),
            Line::from("Navigation"),
            help_line(&Action::MoveDown, "Move down"),
            help_line(&Action::MoveUp, "Move up"),
            help_line(&Action::PageDown, "Scroll down"),
            help_line(&Action::PageUp, "Scroll up"),
            help_line(&Action::First, "First item"),
            help_line(&Action::Last, "Last item"),
            help_line(&Action::OpenContent, "Expand / preview content"),
            help_line(&Action::ShowMetadata, "Toggle metadata panel"),
            Line::from("  Mouse click   Select/expand tree item"),
            Line::from("  Mouse wheel   Scroll tree/metadata"),
            Line::from("  Click link    Open related part"),
            help_line(&Action::ExpandAll, "Expand all"),
            help_line(&Action::CollapseAll, "Collapse all"),
            Line::from(""),
            Line::from("Search"),
            help_line(&Action::StartSearch, "Search package paths"),
            Line::from("  Enter         Select first matching part"),
            help_line(&Action::NextMatch, "Next match"),
            help_line(&Action::PreviousMatch, "Previous match"),
            help_line(&Action::Cancel, "Cancel search/help"),
            Line::from(""),
            help_line(&Action::ToggleHelp, "Show this help"),
            help_line(&Action::Quit, "Quit tree / Vim normal mode"),
            help_line(&Action::QuitEditor, "Quit Emacs editor"),
            help_line(&Action::NavigateBack, "Previous part"),
            help_line(&Action::NavigateForward, "Next part"),
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

pub fn tree_area_contains(area: Rect, app: &App, x: u16, y: u16) -> bool {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(chunks[1]);
    let left = if app.details_visible {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(columns[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)])
            .split(columns[0])
    };
    let tree = left[0];
    x >= tree.x
        && x < tree.x.saturating_add(tree.width)
        && y >= tree.y
        && y < tree.y.saturating_add(tree.height)
}

pub fn metadata_line_at(area: Rect, app: &App, x: u16, y: u16) -> Option<(usize, usize)> {
    if !app.details_visible {
        return None;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(chunks[1]);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(columns[0]);
    let inner = Block::bordered().inner(left[1]);

    if x >= inner.x
        && x < inner.x.saturating_add(inner.width)
        && y >= inner.y
        && y < inner.y.saturating_add(inner.height)
    {
        Some((
            usize::from(y - inner.y) + usize::from(app.details_scroll),
            usize::from(x - inner.x),
        ))
    } else {
        None
    }
}

fn content_title(kind: PreviewKind) -> &'static str {
    match kind {
        PreviewKind::Xml => "XML content",
        PreviewKind::PlainText => "Text content",
        PreviewKind::Json => "JSON preview",
        PreviewKind::Hex => "Hex dump",
        PreviewKind::Image => "Image preview",
        PreviewKind::Info => "Binary information",
        PreviewKind::Error | PreviewKind::Empty => "File content",
    }
}

fn help_line(action: &Action, description: &str) -> Line<'static> {
    let bindings = action.key_bindings_display_with_format(&DisplayFormat::Abbreviation);
    Line::from(format!("  {bindings:<14} {description}"))
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
