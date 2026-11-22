use super::*;

pub(crate) struct TreeView {
    pub(crate) state: TreeState<String>,
    pub(crate) items: Vec<TreeItem<'static, String>>,
    pub(crate) filtered: Option<Vec<TreeItem<'static, String>>>,
    pub(crate) opened_before_search: Option<Vec<Vec<String>>>,
}

pub(super) const EDIT_MARKER: &str = "●";

impl App {
    pub(super) fn install_tree(&mut self) {
        self.tree.filtered = None;
        self.tree.opened_before_search = None;
        match self.build_tree_items() {
            Ok(tree_items) => self.tree.items = tree_items,
            Err(error) => {
                self.tree.items.clear();
                self.worker_error = Some(format!("Could not build package tree: {error}"));
            }
        }
    }

    /// Rebuild the labels after the set of edited parts changed. The live path
    /// filter and the tree's open/closed state are kept, so marking a part does
    /// not throw the user out of a search.
    pub(super) fn refresh_tree_labels(&mut self) {
        if self.tree.items.is_empty() {
            return;
        }
        let Ok(tree_items) = self.build_tree_items() else {
            return;
        };
        self.tree.items = tree_items;
        if !self.search.query.is_empty() {
            let query = self.search.query.to_ascii_lowercase();
            match filter_tree(&self.tree.items, &query) {
                Ok(items) => {
                    for path in collect_open_paths(&items) {
                        self.tree.state.open(path);
                    }
                    self.tree.filtered = Some(items);
                }
                Err(_) => self.tree.filtered = None,
            }
        } else if !self.content_search.query.is_empty() && self.tree.filtered.is_some() {
            self.apply_content_search_filter();
        }
        self.tree.state.scroll_selected_into_view();
    }

    pub(super) fn build_tree_items(&self) -> io::Result<Vec<TreeItem<'static, String>>> {
        // A sorted, de-duplicated union: in compare mode the tree must show
        // added and removed parts, not just the primary package's parts.
        let mut paths: BTreeSet<&String> = self.index().parts.keys().collect();
        if let Some(compare) = self.compare.as_ref() {
            paths.extend(compare.package.index.parts.keys());
        }
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|path| {
                !self.hide_unchanged_parts
                    || self
                        .compare
                        .as_ref()
                        .and_then(|compare| compare.comparison.status_of(path))
                        .is_some_and(|status| status != PartStatus::Unchanged)
            })
            .map(|path| path.trim_start_matches('/'))
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect();
        // Parts with an integrity issue, a comparison difference, or unsaved
        // edits are marked in the tree so they can be spotted without reading
        // the metadata panel.
        let markers = self.tree_markers();
        create_tree(&paths, &markers)
    }

    /// Suffix markers appended to tree labels: integrity warnings, unsaved
    /// edits, and, in compare mode, the part status. Ancestors of a differing
    /// part get a `*` so a collapsed directory still advertises the change.
    pub(super) fn tree_markers(&self) -> HashMap<String, String> {
        let mut markers: HashMap<String, String> = HashMap::new();
        for issue in &self.index().integrity {
            if let Some(part) = issue.part.as_deref() {
                add_marker(&mut markers, part, "⚠");
            }
        }
        for path in self.editor.edits.keys() {
            add_marker(&mut markers, path, EDIT_MARKER);
        }
        if self.editor.dirty
            && let Some(path) = self.editor.previewed_path.as_deref()
        {
            add_marker(&mut markers, path, EDIT_MARKER);
        }
        let Some(compare) = self.compare.as_ref() else {
            return markers;
        };
        for (path, status) in &compare.comparison.statuses {
            if *status == PartStatus::Unchanged {
                continue;
            }
            add_marker(&mut markers, path, status.marker());
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if parent.is_empty() {
                    break;
                }
                add_marker(&mut markers, parent, "*");
                ancestor = parent;
            }
        }
        markers
    }

    pub(super) fn select_path(&mut self, path: &str) {
        let mut identifiers = Vec::new();
        let mut current = String::new();
        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() {
                continue;
            }
            current.push('/');
            current.push_str(component);
            identifiers.push(current.clone());
        }
        for index in 0..identifiers.len().saturating_sub(1) {
            self.tree.state.open(identifiers[..=index].to_vec());
        }
        self.tree.state.select(identifiers);
        self.tree.state.scroll_selected_into_view();
    }

    /// Drop the filtered view and restore the open/closed state the tree had
    /// before the search started.
    pub(super) fn clear_tree_filter(&mut self) {
        self.tree.filtered = None;
        if let Some(opened) = self.tree.opened_before_search.take() {
            self.tree.state.close_all();
            for path in opened {
                self.tree.state.open(path);
            }
        }
    }

    pub fn expand_all(&mut self) {
        let paths = collect_open_paths(self.visible_tree_items());
        for path in paths {
            self.tree.state.open(path);
        }
    }

    pub fn collapse_all(&mut self) {
        self.tree.state.close_all();
        if let Some(first) = self.visible_tree_items().first() {
            let identifier = first.identifier().clone();
            self.tree.state.select(vec![identifier]);
        } else {
            self.tree.state.select(Vec::new());
        }
    }

    /// Tree items currently rendered: the live filter result while searching,
    /// otherwise the full package tree.
    pub fn visible_tree_items(&self) -> &[TreeItem<'static, String>] {
        self.tree.filtered.as_deref().unwrap_or(&self.tree.items)
    }

    /// Whether a search filter currently hides parts of the tree.
    pub fn tree_filter_active(&self) -> bool {
        self.tree.filtered.is_some()
    }
}
/// Build tree items directly from sorted, normalized package paths (no leading
/// slash) without an intermediate node structure.
pub(super) fn create_tree(
    paths: &[String],
    markers: &HashMap<String, String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    create_tree_level("", paths, 0, markers)
}

pub(super) fn collect_open_paths(items: &[TreeItem<'static, String>]) -> Vec<Vec<String>> {
    fn collect(
        items: &[TreeItem<'static, String>],
        parent: &[String],
        paths: &mut Vec<Vec<String>>,
    ) {
        for item in items {
            let mut path = parent.to_vec();
            path.push(item.identifier().clone());
            if !item.children().is_empty() {
                paths.push(path.clone());
                collect(item.children(), &path, paths);
            }
        }
    }

    let mut paths = Vec::new();
    collect(items, &[], &mut paths);
    paths
}

pub(super) fn contains_ignore_ascii_case(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let haystack = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Keep items whose path matches `query`, retaining ancestor directories so
/// matches stay reachable. An item that matches directly keeps its whole
/// subtree. Item text is the final path component, so branches can be rebuilt
/// without access to the original (crate-private) text.
pub(super) fn filter_tree(
    items: &[TreeItem<'static, String>],
    query: &str,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut result = Vec::new();
    for item in items {
        if contains_ignore_ascii_case(item.identifier(), query) {
            result.push(item.clone());
            continue;
        }
        if item.children().is_empty() {
            continue;
        }
        let children = filter_tree(item.children(), query)?;
        if children.is_empty() {
            continue;
        }
        let name = item
            .identifier()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        // Children are a subset of a valid sibling set, so identifiers stay unique.
        let branch =
            TreeItem::new(item.identifier().clone(), name, children).map_err(io::Error::other)?;
        result.push(branch);
    }
    Ok(result)
}

pub(super) fn filter_tree_matches(
    items: &[TreeItem<'static, String>],
    matches: &HashSet<&String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut result = Vec::new();
    for item in items {
        if matches.contains(item.identifier()) {
            result.push(item.clone());
            continue;
        }
        if item.children().is_empty() {
            continue;
        }
        let children = filter_tree_matches(item.children(), matches)?;
        if children.is_empty() {
            continue;
        }
        let name = item
            .identifier()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let branch =
            TreeItem::new(item.identifier().clone(), name, children).map_err(io::Error::other)?;
        result.push(branch);
    }
    Ok(result)
}

/// Tree labels carry any marker for the part: an integrity warning, a
/// comparison status, or `*` for a directory with differing descendants.
pub(super) fn tree_label(
    head: &str,
    identifier: &str,
    markers: &HashMap<String, String>,
) -> String {
    match markers.get(identifier) {
        Some(marker) => format!("{head} {marker}"),
        None => head.to_string(),
    }
}

pub(super) fn styled_tree_label(
    head: &str,
    identifier: &str,
    markers: &HashMap<String, String>,
) -> Line<'static> {
    let label = tree_label(head, identifier, markers);
    let color = markers.get(identifier).and_then(|marker| {
        let has = |symbol| marker.split(' ').any(|part| part == symbol);
        if has("+") {
            Some(Color::LightGreen)
        } else if has("-") {
            Some(Color::LightRed)
        } else if has("~") || has("⚠") || has(EDIT_MARKER) {
            Some(Color::Yellow)
        } else {
            None
        }
    });
    Line::from(Span::styled(
        label,
        color.map_or_else(Style::default, |color| Style::default().fg(color)),
    ))
}

pub(super) fn tree_contains(items: &[TreeItem<'static, String>], path: &str) -> bool {
    items
        .iter()
        .any(|item| item.identifier() == path || tree_contains(item.children(), path))
}

/// Append a marker once, keeping markers space-separated in insertion order.
pub(super) fn add_marker(markers: &mut HashMap<String, String>, path: &str, marker: &str) {
    let entry = markers.entry(path.to_string()).or_default();
    if entry.split(' ').any(|existing| existing == marker) {
        return;
    }
    if !entry.is_empty() {
        entry.push(' ');
    }
    entry.push_str(marker);
}

/// `offset` is the byte length of the shared ancestor prefix including its
/// trailing slash, so recursion never re-allocates path components. Paths are
/// sorted, which groups a directory's children contiguously after it.
pub(super) fn create_tree_level(
    parent: &str,
    paths: &[String],
    offset: usize,
    markers: &HashMap<String, String>,
) -> io::Result<Vec<TreeItem<'static, String>>> {
    let mut items = Vec::new();
    let mut index = 0;
    while index < paths.len() {
        let rest = &paths[index][offset..];
        let head = rest.split('/').next().unwrap_or(rest);
        let identifier = format!("{parent}/{head}");
        let label = styled_tree_label(head, &identifier, markers);
        // A directory entry itself ("head") sorts before its children
        // ("head/..."); consume it so leaf and branch merge into one node.
        if rest.len() == head.len() {
            index += 1;
        }
        let prefix = format!("{head}/");
        let children_start = index;
        while index < paths.len() && paths[index][offset..].starts_with(&prefix) {
            index += 1;
        }
        let children = &paths[children_start..index];
        if children.is_empty() {
            items.push(TreeItem::new_leaf(identifier, label));
        } else {
            let child_offset = offset + head.len() + 1;
            let children = create_tree_level(&identifier, children, child_offset, markers)?;
            items.push(TreeItem::new(identifier, label, children).map_err(io::Error::other)?);
        }
    }
    Ok(items)
}
