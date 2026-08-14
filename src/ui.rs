use crossterm_keybind::{DisplayFormat, KeyBindTrait};
use edtui::{EditorStatusLine, EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation},
    Frame,
};
use ratatui_image::{Resize, StatefulImage};
use tui_tree_widget::Tree;

use crate::{
    app::{App, CurrentWidget},
    keybindings::HelpRow,
    layout::LayoutSnapshot,
    preview::PreviewKind,
    summary::DetailsView,
};

/// Below this size the panel layout cannot render usefully; show a guard instead.
const MIN_TERMINAL_WIDTH: u16 = 40;
const MIN_TERMINAL_HEIGHT: u16 = 12;

pub fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < MIN_TERMINAL_WIDTH || area.height < MIN_TERMINAL_HEIGHT {
        let message = Paragraph::new(format!(
            "Terminal too small: need at least {MIN_TERMINAL_WIDTH}x{MIN_TERMINAL_HEIGHT}"
        ))
        .alignment(Alignment::Center);
        let vertical_center = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Length(1),
                Constraint::Percentage(50),
            ])
            .split(area);
        f.render_widget(message, vertical_center[1]);
        return;
    }

    let snapshot = LayoutSnapshot::new(area, app.details_visible);
    let chunks = [snapshot.header, snapshot.body, snapshot.status];

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
    let sections = [snapshot.tree, snapshot.content];
    let left_sections = [snapshot.tree, snapshot.details.unwrap_or(snapshot.tree)];

    // Tree widget
    let tree_title = if app.tree_filter_active() {
        "[1] Document Inspector (filtered)"
    } else {
        "[1] Document Inspector"
    };
    let tree_block = Block::bordered()
        .title(tree_title)
        .title_top(Line::from("[?] Help").right_aligned())
        .border_style(if app.current_widget == CurrentWidget::Tree {
            active_style
        } else {
            normal_style
        });

    if app.loading {
        f.render_widget(
            Paragraph::new("Loading package…").block(tree_block),
            left_sections[0],
        );
    } else if let Some(error) = app
        .worker_error
        .as_deref()
        .filter(|_| !app.is_package_loaded())
    {
        f.render_widget(
            Paragraph::new(format!("Unable to open package: {error}")).block(tree_block),
            left_sections[0],
        );
    } else {
        // `visible_tree_items` borrows `app` immutably, so the tree state is moved
        // out for the duration of the render and put back afterwards.
        let mut tree_state = std::mem::take(&mut app.tree_state);
        match Tree::new(app.visible_tree_items()) {
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
                f.render_stateful_widget(tree_widget, left_sections[0], &mut tree_state);
            }
            Err(error) => {
                let message = Paragraph::new(format!("Unable to render document tree: {error}"))
                    .block(tree_block);
                f.render_widget(message, left_sections[0]);
            }
        }
        app.tree_state = tree_state;
    }

    if app.details_visible {
        // Copy scalars out first: `details_view` borrows `app` mutably to update
        // the cache, so no other `app` access is allowed while the view is alive.
        let details_focused = app.current_widget == CurrentWidget::Details;
        let raw_scroll = app.details_scroll;
        let raw_cursor = app.details_cursor;
        let details = app.details_view();
        let details_block = Block::bordered()
            .title("[2] Metadata")
            .title_top(Line::from("[d] Hide").right_aligned())
            .border_style(if details_focused {
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
        let details_scroll = raw_scroll.min(max_scroll);
        let details_cursor = if details.links.is_empty() {
            None
        } else {
            Some(raw_cursor.min(details.links.len() - 1))
        };
        let selected_link_line = details_cursor
            .and_then(|cursor| details.links.get(cursor))
            .map(|link| link.line);
        let lines = linked_lines(details, selected_link_line);
        let details = Paragraph::new(Text::from(lines))
            .block(details_block)
            .scroll((details_scroll, 0));
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

    if app.summary_visible {
        if let Some(summary) = app.document_summary.as_ref() {
            let summary_inner = editor_block.inner(sections[1]);
            let max_scroll = summary
                .text
                .lines()
                .count()
                .saturating_sub(summary_inner.height as usize) as u16;
            let summary_scroll = app.summary_scroll.min(max_scroll);
            let summary = Paragraph::new(Text::from(linked_lines(summary, None)))
                .block(editor_block)
                .scroll((summary_scroll, 0));
            f.render_widget(summary, sections[1]);
        }
    } else if let Some(image_state) = app.image_state.as_mut() {
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
    } else if let Some(message) = app.content_message.as_deref() {
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
        let mut lines = Vec::new();
        for (index, (section, rows)) in crate::keybindings::help_sections().iter().enumerate() {
            if index > 0 {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(section.to_string()));
            for row in rows {
                match row {
                    HelpRow::Binding(action, description) => {
                        lines.push(help_line(action, description));
                    }
                    HelpRow::Text(text) => lines.push(Line::from(format!("  {text}"))),
                }
            }
        }
        let help = Paragraph::new(Text::from(lines))
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
    LayoutSnapshot::contains(LayoutSnapshot::new(area, app.details_visible).tree, x, y)
}

pub fn content_area_contains(area: Rect, app: &App, x: u16, y: u16) -> bool {
    LayoutSnapshot::contains(LayoutSnapshot::new(area, app.details_visible).content, x, y)
}

pub fn summary_line_at(area: Rect, app: &App, x: u16, y: u16) -> Option<(usize, usize)> {
    if !app.summary_visible {
        return None;
    }
    LayoutSnapshot::new(area, app.details_visible).content_line(app.summary_scroll, x, y)
}

pub fn metadata_line_at(area: Rect, app: &App, x: u16, y: u16) -> Option<(usize, usize)> {
    if !app.details_visible {
        return None;
    }
    let snapshot = LayoutSnapshot::new(area, true);
    let details = snapshot.details?;
    let inner = Block::bordered().inner(details);
    LayoutSnapshot::contains(inner, x, y).then(|| {
        (
            usize::from(y - inner.y) + usize::from(app.details_scroll),
            usize::from(x - inner.x),
        )
    })
}

/// Render a details/summary view, highlighting link targets. When
/// `selected_link_line` is given (keyboard cursor in the metadata panel), that
/// link is emphasized; otherwise links use the plain link style.
fn linked_lines(view: &DetailsView, selected_link_line: Option<usize>) -> Vec<Line<'static>> {
    view.text
        .lines()
        .enumerate()
        .map(|(line, text)| {
            let Some(link) = view.links.iter().find(|link| link.line == line) else {
                return Line::from(text.to_string());
            };
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
        })
        .collect()
}

fn content_title(kind: PreviewKind) -> &'static str {
    // Editor-backed previews are view-only: edtui has no read-only mode yet, so
    // the title makes it explicit that edits are not saved anywhere.
    match kind {
        PreviewKind::Xml => "XML content (read-only)",
        PreviewKind::PlainText => "Text content (read-only)",
        PreviewKind::Json => "JSON preview (read-only)",
        PreviewKind::Hex => "Hex dump",
        PreviewKind::Image => "Image preview",
        PreviewKind::Summary => "Document summary",
        PreviewKind::Info => "Binary information",
        PreviewKind::Error | PreviewKind::Empty => "File content",
    }
}

fn help_line(action: &crate::keybindings::Action, description: &str) -> Line<'static> {
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
