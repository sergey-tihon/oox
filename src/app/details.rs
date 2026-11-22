use super::*;

pub(crate) struct DetailsPanel {
    pub(crate) visible: bool,
    pub(crate) scroll: u16,
    pub(crate) cursor: usize,
    pub(crate) cache: DetailsView,
    pub(crate) cache_key: Option<(u64, Option<String>)>,
    pub(crate) generation: u64,
}

pub(crate) struct SummaryPanel {
    pub(crate) view: Option<DetailsView>,
    pub(crate) visible: bool,
    pub(crate) scroll: u16,
}

const MAX_INTEGRITY_LINES: usize = 50;

pub(super) fn part_kind_label(kind: &PartKind) -> &'static str {
    match kind {
        PartKind::Xml => "XML",
        PartKind::Image => "Image",
        PartKind::Binary => "Binary/unsupported",
        PartKind::Directory => "Directory",
    }
}

pub(super) fn relationship_target_label(relationship: &Relationship) -> String {
    match relationship.target_mode {
        TargetMode::External => format!("{} (external)", relationship.target),
        TargetMode::Internal => relationship
            .resolved_target
            .clone()
            .unwrap_or_else(|| relationship.target.clone()),
    }
}

pub(super) fn relationship_type_label(relationship: &Relationship) -> String {
    relationship
        .relationship_type
        .rsplit('/')
        .next()
        .unwrap_or(&relationship.relationship_type)
        .to_string()
}

pub(super) fn compact_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let compact = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

pub(super) fn push_detail_line(text: &mut String, line: &str) -> usize {
    let line_number = text.lines().count();
    text.push_str(line);
    text.push('\n');
    line_number
}

pub(super) fn append_package_diagnostics(
    text: &mut String,
    links: &mut Vec<DetailLink>,
    index: &PackageIndex,
    side: Option<&str>,
) {
    if !index.warnings.is_empty() {
        push_detail_line(text, "");
        push_detail_line(
            text,
            &side.map_or_else(|| "Warnings".to_string(), |side| format!("{side} warnings")),
        );
        for warning in &index.warnings {
            push_detail_line(text, &format!("- {}", compact_text(warning, 56)));
        }
    }

    if !index.integrity.is_empty() {
        push_detail_line(text, "");
        let prefix = side.map_or_else(String::new, |side| format!("{side} "));
        let jump = if side.is_none() || side == Some("A") {
            "  [i/I jump]"
        } else {
            ""
        };
        push_detail_line(
            text,
            &format!("{prefix}Integrity issues ({}){jump}", index.integrity.len()),
        );
        for issue in index.integrity.iter().take(MAX_INTEGRITY_LINES) {
            let severity = match issue.severity {
                DiagnosticSeverity::Error => "error",
                DiagnosticSeverity::Warning => "warn",
            };
            let prefix = format!("  [{severity}] ");
            let message = compact_text(&issue.message, 64);
            let line_number = push_detail_line(text, &format!("{prefix}{message}"));
            if let Some(part) = issue.part.as_deref() {
                let start = prefix.chars().count();
                links.push(DetailLink {
                    line: line_number,
                    start,
                    end: start + message.chars().count(),
                    target: part.to_string(),
                });
            }
        }
        if index.integrity.len() > MAX_INTEGRITY_LINES {
            push_detail_line(
                text,
                &format!("  … {} more", index.integrity.len() - MAX_INTEGRITY_LINES),
            );
        }
    }
}

pub(super) fn append_comparison_diagnostics(
    text: &mut String,
    comparison: &Comparison,
    selected: &str,
) {
    let diagnostics: Vec<&Diagnostic> = comparison
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .part
                .as_deref()
                .is_none_or(|path| path == selected)
        })
        .collect();
    if diagnostics.is_empty() {
        return;
    }
    push_detail_line(text, "");
    push_detail_line(text, "Comparison diagnostics");
    for diagnostic in diagnostics {
        let severity = match diagnostic.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warn",
        };
        push_detail_line(
            text,
            &format!(
                "- [{severity}] {}: {}",
                diagnostic.stage,
                compact_text(&diagnostic.message, 56)
            ),
        );
    }
}

pub(super) fn append_comparison_side(
    text: &mut String,
    side: &str,
    index: &PackageIndex,
    selected: &str,
) {
    let package_name = index
        .source
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("Package {side}"));
    push_detail_line(text, &format!("{side} — {package_name}"));

    let Some(part) = index.parts.get(selected) else {
        if index.is_directory(selected) {
            push_detail_line(text, "  Kind: Directory");
            return;
        }
        push_detail_line(text, "  Part: Absent");
        return;
    };

    push_detail_line(text, &format!("  Kind: {}", part_kind_label(&part.kind)));
    push_detail_line(
        text,
        &format!(
            "  Content type: {}",
            part.content_type.as_deref().unwrap_or("Unknown")
        ),
    );
    push_detail_line(
        text,
        &format!(
            "  Size: {} bytes ({} compressed)",
            part.size, part.compressed_size
        ),
    );
    let outgoing = index.outgoing.get(selected).map_or(0, Vec::len);
    let incoming = index.incoming.get(selected).map_or(0, Vec::len);
    push_detail_line(
        text,
        &format!("  Relationships: {outgoing} out, {incoming} in"),
    );
}

pub(super) type RelationshipLink = (usize, usize, String);
pub(super) type ComparisonRelationshipRow = (String, Option<RelationshipLink>);
pub(super) type ComparisonRelationshipRows = BTreeMap<String, ComparisonRelationshipRow>;

pub(super) fn comparison_relationships(
    index: &PackageIndex,
    selected: &str,
) -> ComparisonRelationshipRows {
    let mut rows = BTreeMap::new();
    for relationship_index in index.outgoing.get(selected).into_iter().flatten() {
        let relationship = &index.relationships[*relationship_index];
        let target = compact_text(&relationship_target_label(relationship), 48);
        let relationship_type = relationship_type_label(relationship);
        let display = format!("OUT {}  {target} ({relationship_type})", relationship.id);
        let start = 4 + relationship.id.chars().count() + 2;
        let link = relationship
            .resolved_target
            .as_ref()
            .map(|target_path| (start, start + target.chars().count(), target_path.clone()));
        let key = format!(
            "out\0{}\0{}\0{}\0{}\0{:?}",
            relationship.id,
            relationship.source,
            relationship.target,
            relationship.relationship_type,
            relationship.target_mode
        );
        rows.insert(key, (display, link));
    }
    for relationship_index in index.incoming.get(selected).into_iter().flatten() {
        let relationship = &index.relationships[*relationship_index];
        let source = compact_text(&relationship.source, 48);
        let relationship_type = relationship_type_label(relationship);
        let display = format!("IN {}  {source} ({relationship_type})", relationship.id);
        let start = 3 + relationship.id.chars().count() + 2;
        let link = Some((
            start,
            start + source.chars().count(),
            relationship.source.clone(),
        ));
        let key = format!(
            "in\0{}\0{}\0{}\0{}\0{:?}",
            relationship.id,
            relationship.source,
            relationship.target,
            relationship.relationship_type,
            relationship.target_mode
        );
        rows.insert(key, (display, link));
    }
    rows
}

pub(super) fn append_comparison_relationships(
    text: &mut String,
    links: &mut Vec<DetailLink>,
    index_a: &PackageIndex,
    index_b: &PackageIndex,
    selected: &str,
) {
    let rows_a = comparison_relationships(index_a, selected);
    let rows_b = comparison_relationships(index_b, selected);
    if rows_a.is_empty() && rows_b.is_empty() {
        return;
    }

    push_detail_line(text, "");
    push_detail_line(text, "Related parts  [= shared, A/B side-specific]");
    let mut add_row = |side: &str, (display, link): &ComparisonRelationshipRow| {
        let prefix = format!("  [{side}] ");
        let line_number = push_detail_line(text, &format!("{prefix}{display}"));
        if let Some((start, end, target)) = link {
            let prefix_len = prefix.chars().count();
            links.push(DetailLink {
                line: line_number,
                start: prefix_len + start,
                end: prefix_len + end,
                target: target.clone(),
            });
        }
    };

    for (key, row) in &rows_a {
        add_row(if rows_b.contains_key(key) { "=" } else { "A" }, row);
    }
    for (key, row) in &rows_b {
        if !rows_a.contains_key(key) {
            add_row("B", row);
        }
    }
}

impl App {
    pub fn toggle_details(&mut self) {
        self.details.visible = !self.details.visible;
        if self.details.visible {
            self.details.scroll = 0;
            self.details.cursor = 0;
        }
    }

    pub fn toggle_summary(&mut self) -> io::Result<()> {
        if self.summary.visible {
            self.summary.visible = false;
            return self.load_selected_file_content_inner(false);
        }

        if self.summary.view.is_none() {
            self.preview.kind = PreviewKind::Error;
            self.content_message = Some("No document-specific summary is available".to_string());
            return Ok(());
        }

        self.summary.visible = true;
        self.summary.scroll = 0;
        self.preview.image_state = None;
        // The buffer is set aside rather than discarded, so an unsaved edit
        // survives a detour through the summary view.
        self.stash_current_edit();
        self.editor.state = self.new_editor_state(Lines::default());
        self.editor.baseline = None;
        self.editor.editable = false;
        self.preview.kind = PreviewKind::Summary;
        self.content_message = None;
        Ok(())
    }

    /// The metadata view for the current selection. The view is cached and only
    /// rebuilt when the selection or the loaded package changes, so callers may
    /// invoke it freely (per frame, per cursor move) without allocation churn.
    pub fn details_view(&mut self) -> &DetailsView {
        let key = (
            self.details.generation,
            self.tree.state.selected().last().cloned(),
        );
        if self.details.cache_key.as_ref() != Some(&key) {
            self.details.cache = self.build_details_view();
            self.details.cache_key = Some(key);
        }
        &self.details.cache
    }

    pub(super) fn build_details_view(&self) -> DetailsView {
        let Some(selected) = self.tree.state.selected().last() else {
            return DetailsView {
                text: "Select a package part to see metadata\n".to_string(),
                links: Vec::new(),
            };
        };

        let index = self.index();
        let mut text = String::new();
        let mut links = Vec::new();
        let display_name = selected.trim_start_matches('/');
        push_detail_line(&mut text, &format!("Part: {display_name}"));

        if let Some(compare) = self.compare.as_ref() {
            if let Some(status) = compare.comparison.status_of(selected) {
                push_detail_line(&mut text, &format!("Diff: {}", status.label()));
            }
            push_detail_line(&mut text, &compare.comparison.summary_line());
            push_detail_line(&mut text, "");
            append_comparison_side(&mut text, "A", index, selected);
            append_package_diagnostics(&mut text, &mut links, index, Some("A"));
            push_detail_line(&mut text, "");
            append_comparison_side(&mut text, "B", &compare.package.index, selected);
            append_package_diagnostics(&mut text, &mut links, &compare.package.index, Some("B"));
            append_comparison_diagnostics(&mut text, &compare.comparison, selected);
            append_comparison_relationships(
                &mut text,
                &mut links,
                index,
                &compare.package.index,
                selected,
            );
            return DetailsView { text, links };
        }

        if let Some(part) = index.parts.get(selected) {
            if part.archive_name != display_name {
                push_detail_line(&mut text, &format!("Archive: {}", part.archive_name));
            }
            push_detail_line(&mut text, &format!("Kind: {}", part_kind_label(&part.kind)));
            push_detail_line(
                &mut text,
                &format!(
                    "Content type: {}",
                    part.content_type.as_deref().unwrap_or("Unknown")
                ),
            );
            push_detail_line(
                &mut text,
                &format!(
                    "Size: {} bytes ({} compressed)",
                    part.size, part.compressed_size
                ),
            );
        } else if self.is_directory(selected) {
            push_detail_line(&mut text, "Kind: Directory");
            push_detail_line(&mut text, "Content type: N/A");
        } else {
            push_detail_line(&mut text, "Kind: Unavailable");
            push_detail_line(&mut text, "Content type: Unknown");
        }

        push_detail_line(&mut text, "");
        let outgoing = index.outgoing.get(selected);
        let incoming = index.incoming.get(selected);
        push_detail_line(
            &mut text,
            &format!(
                "Relationships: {} outgoing, {} incoming",
                outgoing.map_or(0, Vec::len),
                incoming.map_or(0, Vec::len)
            ),
        );

        if let Some(relationships) = outgoing {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Outgoing");
            for relationship_index in relationships {
                let relationship = &index.relationships[*relationship_index];
                let target_label = compact_text(&relationship_target_label(relationship), 48);
                let relationship_type = relationship_type_label(relationship);
                let line_number = push_detail_line(
                    &mut text,
                    &format!(
                        "  {}  {} ({relationship_type})",
                        relationship.id, target_label
                    ),
                );
                if let Some(target) = relationship.resolved_target.as_ref() {
                    let start = 2 + relationship.id.chars().count() + 2;
                    links.push(DetailLink {
                        line: line_number,
                        start,
                        end: start + target_label.chars().count(),
                        target: target.clone(),
                    });
                }
            }
        }

        if let Some(relationships) = incoming {
            push_detail_line(&mut text, "");
            push_detail_line(&mut text, "Incoming");
            for relationship_index in relationships {
                let relationship = &index.relationships[*relationship_index];
                let source = compact_text(&relationship.source, 48);
                let relationship_type = relationship_type_label(relationship);
                let line_number = push_detail_line(
                    &mut text,
                    &format!("  {}  {} ({relationship_type})", relationship.id, source),
                );
                let start = 2 + relationship.id.chars().count() + 2;
                links.push(DetailLink {
                    line: line_number,
                    start,
                    end: start + source.chars().count(),
                    target: relationship.source.clone(),
                });
            }
        }

        append_package_diagnostics(&mut text, &mut links, index, None);

        DetailsView { text, links }
    }

    pub fn scroll_details(&mut self, amount: i16) {
        if amount.is_negative() {
            self.details.scroll = self.details.scroll.saturating_sub(amount.unsigned_abs());
        } else {
            self.details.scroll = self.details.scroll.saturating_add(amount as u16);
        }
    }

    pub fn move_details_cursor(&mut self, reverse: bool) {
        let links_len = self.details_view().links.len();
        if links_len == 0 {
            self.scroll_details(if reverse { -1 } else { 1 });
            return;
        }

        let current = self.details.cursor.min(links_len - 1);
        self.details.cursor = if reverse {
            current.checked_sub(1).unwrap_or(links_len - 1)
        } else {
            (current + 1) % links_len
        };
        let cursor = self.details.cursor;
        let line = self.details_view().links[cursor].line;
        self.details.scroll = line.saturating_sub(2) as u16;
    }

    pub fn activate_current_detail_link(&mut self) -> io::Result<bool> {
        let cursor = self.details.cursor;
        let Some((line, start)) = self
            .details_view()
            .links
            .get(cursor)
            .map(|link| (link.line, link.start))
        else {
            return Ok(false);
        };
        self.activate_detail_link(line, start)
    }

    pub fn activate_detail_link(&mut self, line: usize, column: usize) -> io::Result<bool> {
        let target = {
            let view = self.details_view();
            let Some(link) = view
                .links
                .iter()
                .find(|link| link.line == line && column >= link.start && column < link.end)
            else {
                return Ok(false);
            };
            link.target.clone()
        };

        let target_exists = self.index().parts.contains_key(&target)
            || self
                .compare
                .as_ref()
                .is_some_and(|compare| compare.package.index.parts.contains_key(&target));
        if !target_exists && !self.is_directory(&target) {
            return Ok(false);
        }
        // An applied filter could hide the destination, so a link that selected
        // an invisible item would look like it did nothing.
        self.cancel_any_search();
        if self.hide_unchanged_parts && !tree_contains(&self.tree.items, &target) {
            self.hide_unchanged_parts = false;
            self.install_tree();
        }
        self.select_path(&target);
        self.details.scroll = 0;
        self.load_selected_file_content()?;
        Ok(true)
    }

    pub fn scroll_summary(&mut self, amount: i16) {
        if amount.is_negative() {
            self.summary.scroll = self.summary.scroll.saturating_sub(amount.unsigned_abs());
        } else {
            self.summary.scroll = self.summary.scroll.saturating_add(amount as u16);
        }
    }

    pub fn activate_summary_link(&mut self, line: usize, column: usize) -> io::Result<bool> {
        let Some(summary) = self.summary.view.as_ref() else {
            return Ok(false);
        };
        let Some(target) = summary
            .links
            .iter()
            .find(|link| link.line == line && column >= link.start && column < link.end)
            .map(|link| link.target.clone())
        else {
            return Ok(false);
        };

        if !self.index().parts.contains_key(&target) && !self.is_directory(&target) {
            return Ok(false);
        }
        self.cancel_any_search();
        self.summary.visible = false;
        self.summary.scroll = 0;
        self.select_path(&target);
        self.load_selected_file_content_inner(true)?;
        Ok(true)
    }
}
