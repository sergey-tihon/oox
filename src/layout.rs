use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Block,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutSnapshot {
    pub header: Rect,
    pub body: Rect,
    pub status: Rect,
    pub tree: Rect,
    pub details: Option<Rect>,
    pub content: Rect,
    pub content_inner: Rect,
}

impl LayoutSnapshot {
    pub fn new(area: Rect, details_visible: bool) -> Self {
        let sections = Layout::default()
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
            .split(sections[1]);
        let left = if details_visible {
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
        let details = details_visible.then(|| left[1]);
        Self {
            header: sections[0],
            body: sections[1],
            status: sections[2],
            tree: left[0],
            details,
            content: columns[1],
            content_inner: Block::bordered().inner(columns[1]),
        }
    }

    pub fn contains(rect: Rect, x: u16, y: u16) -> bool {
        x >= rect.x
            && x < rect.x.saturating_add(rect.width)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.height)
    }

    pub fn content_line(&self, scroll: u16, x: u16, y: u16) -> Option<(usize, usize)> {
        Self::contains(self.content_inner, x, y).then(|| {
            (
                usize::from(y - self.content_inner.y) + usize::from(scroll),
                usize::from(x - self.content_inner.x),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hit_testing_matches_snapshot() {
        let layout = LayoutSnapshot::new(
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 30,
            },
            true,
        );
        assert!(LayoutSnapshot::contains(
            layout.tree,
            layout.tree.x,
            layout.tree.y
        ));
        assert!(!LayoutSnapshot::contains(layout.tree, 99, 29));
        assert!(layout
            .content_line(2, layout.content_inner.x, layout.content_inner.y)
            .is_some());
    }
}
