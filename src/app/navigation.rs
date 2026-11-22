use super::*;

/// Bounds the back/forward navigation history so long sessions cannot grow it
/// without limit.
pub(super) const MAX_NAVIGATION_HISTORY: usize = 256;

pub(crate) struct NavigationHistory {
    pub(crate) back: VecDeque<String>,
    pub(crate) forward: Vec<String>,
    pub(crate) current: Option<String>,
}

impl App {
    pub(super) fn record_navigation(&mut self, selected: &str) {
        if self.history.current.as_deref() == Some(selected) {
            return;
        }
        if let Some(current) = self.history.current.replace(selected.to_string()) {
            self.history.back.push_back(current);
            while self.history.back.len() > MAX_NAVIGATION_HISTORY {
                self.history.back.pop_front();
            }
        }
        self.history.forward.clear();
    }

    pub fn navigate_back(&mut self) -> io::Result<bool> {
        let Some(previous) = self.history.back.pop_back() else {
            return Ok(false);
        };
        if let Some(current) = self.history.current.clone() {
            self.history.forward.push(current);
        }
        self.select_path(&previous);
        self.load_selected_file_content_inner(false)?;
        self.history.current = Some(previous);
        Ok(true)
    }

    pub fn navigate_forward(&mut self) -> io::Result<bool> {
        let Some(next) = self.history.forward.pop() else {
            return Ok(false);
        };
        if let Some(current) = self.history.current.clone() {
            self.history.back.push_back(current);
            while self.history.back.len() > MAX_NAVIGATION_HISTORY {
                self.history.back.pop_front();
            }
        }
        self.select_path(&next);
        self.load_selected_file_content_inner(false)?;
        self.history.current = Some(next);
        Ok(true)
    }
}
