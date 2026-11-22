use super::tree::contains_ignore_ascii_case;
use super::*;

pub(crate) struct PathSearch {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) matches: Vec<String>,
    pub(crate) index: Option<usize>,
}

pub(crate) struct ContentSearch {
    pub(crate) active: bool,
    pub(crate) query: String,
    pub(crate) matches: Vec<String>,
    pub(crate) index: Option<usize>,
    pub(crate) request_id: u64,
    pub(crate) pending: bool,
}

const MAX_CONTENT_SEARCH_QUERY_CHARS: usize = 256;

impl App {
    pub fn start_search(&mut self) {
        if self.content_search.active || !self.content_search.query.is_empty() {
            self.cancel_content_search();
        }
        self.search.active = true;
        if self
            .content_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.content_message = None;
        }
        self.search.query.clear();
        self.search.matches.clear();
        self.search.index = None;
        self.tree.filtered = None;
        // Keep the earliest snapshot so re-entering search while a filter is
        // applied still restores the original open/closed state on cancel.
        if self.tree.opened_before_search.is_none() {
            self.tree.opened_before_search =
                Some(self.tree.state.opened().iter().cloned().collect());
        }
    }

    pub fn search_input_char(&mut self, character: char) {
        self.search.query.push(character);
        self.update_tree_filter(true);
    }

    pub fn search_backspace(&mut self) {
        self.search.query.pop();
        self.update_tree_filter(false);
    }

    pub fn finish_search(&mut self) {
        self.search.active = false;
        self.update_search_matches();
        if self.search.query.is_empty() {
            self.tree.opened_before_search = None;
        }
    }

    pub fn cancel_search(&mut self) {
        self.search.active = false;
        if self
            .content_message
            .as_deref()
            .is_some_and(|message| message.starts_with("No package parts match:"))
        {
            self.content_message = None;
        }
        self.search.query.clear();
        self.search.matches.clear();
        self.search.index = None;
        self.clear_tree_filter();
    }

    /// Recompute the live filter, open the retained branches so matches are
    /// visible, and move the selection to the first match.
    pub(super) fn update_tree_filter(&mut self, narrow: bool) {
        let query = self.search.query.to_ascii_lowercase();
        if query.is_empty() {
            self.tree.filtered = None;
            self.search.matches.clear();
            self.search.index = None;
            return;
        }
        let source = if narrow {
            self.tree.filtered.as_deref().unwrap_or(&self.tree.items)
        } else {
            &self.tree.items
        };
        match filter_tree(source, &query) {
            Ok(items) => {
                let paths = collect_open_paths(&items);
                self.tree.filtered = Some(items);
                for path in paths {
                    self.tree.state.open(path);
                }
            }
            Err(error) => {
                self.tree.filtered = None;
                self.content_message = Some(format!("Could not filter tree: {error}"));
                return;
            }
        }
        self.update_search_matches();
    }

    pub fn next_search_match(&mut self, reverse: bool) {
        if self.search.matches.is_empty() {
            self.update_search_matches();
        }
        if self.search.matches.is_empty() {
            return;
        }

        let current = self.search.index.unwrap_or(0);
        let next = if reverse {
            if current == 0 {
                self.search.matches.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % self.search.matches.len()
        };
        self.search.index = Some(next);
        let path = self.search.matches[next].clone();
        self.select_path(&path);
    }

    pub fn start_content_search(&mut self) {
        if self.search.active || !self.search.query.is_empty() {
            self.cancel_search();
        }
        if self.content_search.active || !self.content_search.query.is_empty() {
            self.cancel_content_search();
        }
        self.content_search.active = true;
        self.content_search.query.clear();
        self.content_search.matches.clear();
        self.content_search.index = None;
        self.tree.filtered = None;
        if self.tree.opened_before_search.is_none() {
            self.tree.opened_before_search =
                Some(self.tree.state.opened().iter().cloned().collect());
        }
    }

    pub fn content_search_input_char(&mut self, character: char) {
        if self.content_search.query.chars().count() >= MAX_CONTENT_SEARCH_QUERY_CHARS {
            return;
        }
        self.content_search.query.push(character);
        self.submit_content_search();
    }

    pub fn content_search_backspace(&mut self) {
        self.content_search.query.pop();
        self.submit_content_search();
    }

    pub fn finish_content_search(&mut self) {
        self.content_search.active = false;
        if self.content_search.query.is_empty() {
            self.tree.opened_before_search = None;
        }
    }

    pub fn cancel_content_search(&mut self) {
        self.content_search.active = false;
        self.worker.cancel_search();
        self.content_search.request_id = self.content_search.request_id.wrapping_add(1);
        self.content_search.pending = false;
        self.content_search.query.clear();
        self.content_search.matches.clear();
        self.content_search.index = None;
        if self.content_message.as_deref().is_some_and(|message| {
            message.starts_with("No package contents match:")
                || message.starts_with("Content search failed:")
        }) {
            self.content_message = None;
        }
        self.clear_tree_filter();
    }

    pub fn has_content_search_query(&self) -> bool {
        !self.content_search.query.is_empty()
    }

    pub fn cancel_any_search(&mut self) {
        if self.content_search.active || !self.content_search.query.is_empty() {
            self.cancel_content_search();
        } else if self.search.active || !self.search.query.is_empty() {
            self.cancel_search();
        }
    }

    pub fn next_content_search_match(&mut self, reverse: bool) {
        if self.content_search.matches.is_empty() {
            return;
        }
        let current = self.content_search.index.unwrap_or(0);
        let next = if reverse {
            if current == 0 {
                self.content_search.matches.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % self.content_search.matches.len()
        };
        self.content_search.index = Some(next);
        let path = self.content_search.matches[next].clone();
        self.select_path(&path);
    }

    pub(super) fn submit_content_search(&mut self) {
        self.content_search.request_id = self.content_search.request_id.wrapping_add(1);
        self.content_search.pending = false;
        self.content_search.matches.clear();
        self.content_search.index = None;
        self.tree.filtered = None;
        if self.content_search.query.is_empty() {
            self.worker.cancel_search();
            return;
        }
        let Some((package_source, index)) = self
            .package
            .as_ref()
            .map(|package| (package.source.clone(), Arc::clone(&package.index)))
        else {
            self.content_message = Some("Package is still loading".to_string());
            return;
        };
        let request_id = self.content_search.request_id;
        if let Err(error) = self.worker.submit(Job::SearchContent {
            request_id,
            package_path: package_source,
            query: self.content_search.query.clone(),
            index,
        }) {
            self.content_message = Some(format!("Content search failed: {error}"));
            return;
        }
        self.content_search.pending = true;
        self.content_message = Some(format!(
            "Searching package contents for: {}…",
            self.content_search.query
        ));
    }

    pub(super) fn apply_content_search_filter(&mut self) {
        let matches = self.content_search.matches.iter().collect::<HashSet<_>>();
        match filter_tree_matches(&self.tree.items, &matches) {
            Ok(items) => {
                let open_paths = collect_open_paths(&items);
                self.tree.filtered = Some(items);
                for path in open_paths {
                    self.tree.state.open(path);
                }
            }
            Err(error) => {
                self.tree.filtered = None;
                self.content_message = Some(format!("Could not filter tree: {error}"));
            }
        }
    }

    pub(super) fn update_search_matches(&mut self) {
        let query = self.search.query.to_ascii_lowercase();
        if query.is_empty() {
            self.search.matches.clear();
            self.search.index = None;
            return;
        }

        let mut paths: BTreeSet<&String> = self.index().parts.keys().collect();
        if let Some(compare) = self.compare.as_ref() {
            paths.extend(compare.package.index.parts.keys());
        }
        let matches: Vec<String> = paths
            .into_iter()
            .filter(|path| contains_ignore_ascii_case(path, &query))
            .cloned()
            .collect();
        self.search.matches = matches;

        if self.search.matches.is_empty() {
            self.search.index = None;
            self.content_message = Some(format!("No package parts match: {}", self.search.query));
            return;
        }

        self.search.index = Some(0);
        let path = self.search.matches[0].clone();
        self.select_path(&path);
    }
}
