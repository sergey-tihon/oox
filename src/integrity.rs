//! OPC package integrity checks (issue #12, phase 1).
//!
//! Pure structural validation of the metadata already collected in
//! [`PackageIndex`]: dangling relationship targets, parts without a content
//! type, orphan parts, duplicate relationship ids, and missing required parts.
//! No XML schema validation happens here.
use std::collections::{BTreeMap, BTreeSet};

use crate::package::{Diagnostic, PackageIndex, PartKind, TargetMode};

const CONTENT_TYPES_PART: &str = "/[Content_Types].xml";
const ROOT_RELS_PART: &str = "/_rels/.rels";

/// Every structural problem found in a loaded package, in a stable order.
pub fn check(index: &PackageIndex) -> Vec<Diagnostic> {
    let mut issues = Vec::new();
    if !index.parts.contains_key(CONTENT_TYPES_PART) {
        issues.push(Diagnostic::error(
            "integrity",
            None,
            format!("package is missing {CONTENT_TYPES_PART}"),
        ));
    }
    let has_root_rels = index.parts.contains_key(ROOT_RELS_PART);
    if !has_root_rels {
        issues.push(Diagnostic::error(
            "integrity",
            None,
            format!("package is missing {ROOT_RELS_PART}"),
        ));
    } else if !has_office_document(index) {
        issues.push(Diagnostic::error(
            "integrity",
            Some(ROOT_RELS_PART.to_string()),
            "root relationships declare no officeDocument part",
        ));
    }

    check_relationships(index, &mut issues);
    check_content_types(index, &mut issues);
    if has_root_rels {
        check_orphans(index, &mut issues);
    }
    issues
}

fn has_office_document(index: &PackageIndex) -> bool {
    relationships_from(index, "/").any(|relationship| {
        relationship.target_mode == TargetMode::Internal
            && relationship.relationship_type.rsplit('/').next() == Some("officeDocument")
    })
}

fn relationships_from<'a>(
    index: &'a PackageIndex,
    source: &str,
) -> impl Iterator<Item = &'a crate::package::Relationship> {
    index
        .outgoing
        .get(source)
        .into_iter()
        .flatten()
        .filter_map(|relationship_index| index.relationships.get(*relationship_index))
}

fn check_relationships(index: &PackageIndex, issues: &mut Vec<Diagnostic>) {
    for relationship in &index.relationships {
        if relationship.target_mode == TargetMode::External {
            continue;
        }
        let target = relationship
            .resolved_target
            .as_deref()
            .unwrap_or(&relationship.target);
        if !index.parts.contains_key(target) {
            let relationship_type = relationship
                .relationship_type
                .rsplit('/')
                .next()
                .unwrap_or(&relationship.relationship_type);
            issues.push(Diagnostic::error(
                "integrity",
                owner_part(index, &relationship.source),
                format!(
                    "{} relationship {} targets missing part {target}",
                    relationship_type, relationship.id
                ),
            ));
        }
    }

    // A duplicate id makes every reference to it ambiguous. Report each id once
    // rather than once per occurrence.
    let mut occurrences: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for relationship in &index.relationships {
        *occurrences
            .entry((&relationship.source, &relationship.id))
            .or_default() += 1;
    }
    for ((source, id), count) in occurrences {
        if count > 1 {
            issues.push(Diagnostic::error(
                "integrity",
                owner_part(index, source),
                format!("relationship id {id} is declared {count} times"),
            ));
        }
    }
}

fn check_content_types(index: &PackageIndex, issues: &mut Vec<Diagnostic>) {
    // `.rels` parts are exempt from the reachability rule, not from this one: a
    // manifest without a `rels` default leaves them untyped, which is exactly
    // the corruption worth reporting.
    for part in index.parts.values() {
        if part.kind == PartKind::Directory || is_reserved_part(&part.path) {
            continue;
        }
        if part.content_type.is_none() {
            issues.push(Diagnostic::error(
                "integrity",
                Some(part.path.clone()),
                "part has no content type (no Override, no Default for its extension)",
            ));
        }
    }
}

/// Parts must be reachable from `/_rels/.rels` by following internal
/// relationships. `[Content_Types].xml` and `.rels` parts are implicit in OPC
/// and are never relationship targets, so they are not orphans.
fn check_orphans(index: &PackageIndex, issues: &mut Vec<Diagnostic>) {
    let mut reachable = BTreeSet::new();
    let mut queue = vec!["/".to_string()];
    while let Some(source) = queue.pop() {
        for relationship in relationships_from(index, &source) {
            let Some(target) = relationship.resolved_target.as_deref() else {
                continue;
            };
            if index.parts.contains_key(target) && reachable.insert(target.to_string()) {
                queue.push(target.to_string());
            }
        }
    }

    for (path, part) in &index.parts {
        if is_reserved_part(path)
            || part.kind == PartKind::Directory
            || part.archive_name.to_ascii_lowercase().ends_with(".rels")
        {
            continue;
        }
        if !reachable.contains(path) {
            issues.push(Diagnostic::warning(
                "integrity",
                Some(path.clone()),
                format!("part is not reachable from {ROOT_RELS_PART}"),
            ));
        }
    }
}

/// OPC reserves bracket-delimited names such as `[Content_Types].xml` and
/// `[trash]`. They are not relationship targets and the content-type rules do
/// not apply, so both checks skip them.
fn is_reserved_part(path: &str) -> bool {
    path.split('/').any(|component| component.starts_with('['))
}

/// The part a relationship problem belongs to: its source part, or the `.rels`
/// part that declares it when the source itself is not a packaged part.
fn owner_part(index: &PackageIndex, source: &str) -> Option<String> {
    let candidate = if source == "/" {
        ROOT_RELS_PART.to_string()
    } else {
        source.to_string()
    };
    if index.parts.contains_key(&candidate) {
        return Some(candidate);
    }
    let rels = rels_part(source);
    index.parts.contains_key(&rels).then_some(rels)
}

fn rels_part(source: &str) -> String {
    if source == "/" {
        return ROOT_RELS_PART.to_string();
    }
    match source.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/_rels/{name}.rels"),
        None => format!("/_rels/{source}.rels"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    /// Build an in-memory package and index it the same way `Package::open` does.
    fn package(entries: &[(&str, &str)]) -> PackageIndex {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        PackageIndex::from_archive(&mut archive).unwrap()
    }

    const CONTENT_TYPES: &str = r#"<?xml version="1.0"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#;

    const ROOT_RELS: &str = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#;

    fn messages(index: &PackageIndex) -> Vec<String> {
        index
            .integrity
            .iter()
            .map(|issue| issue.message.clone())
            .collect()
    }

    fn part_of(index: &PackageIndex, needle: &str) -> Option<String> {
        index
            .integrity
            .iter()
            .find(|issue| issue.message.contains(needle))
            .and_then(|issue| issue.part.clone())
    }

    #[test]
    fn minimal_package_without_relationships_is_clean() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("ppt/presentation.xml", "<p/>"),
        ]);
        // `ppt/presentation.xml` is declared by the root rels in a real package;
        // without them the package is only missing the officeDocument part.
        assert_eq!(
            messages(&index),
            vec!["package is missing /_rels/.rels".to_string()]
        );
    }

    #[test]
    fn dangling_target_is_an_error_owned_by_its_source_part() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#,
            ),
        ]);
        assert_eq!(
            part_of(&index, "targets missing part /ppt/slides/slide1.xml").as_deref(),
            Some("/ppt/presentation.xml")
        );
    }

    #[test]
    fn part_without_a_content_type_is_flagged() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
            ("ppt/notes.txt", "notes"),
        ]);
        assert_eq!(
            part_of(&index, "no content type").as_deref(),
            Some("/ppt/notes.txt")
        );
    }

    #[test]
    fn unreachable_parts_are_orphans() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
            ("ppt/slides/slide1.xml", "<s/>"),
        ]);
        assert_eq!(
            part_of(&index, "not reachable").as_deref(),
            Some("/ppt/slides/slide1.xml")
        );
    }

    #[test]
    fn duplicate_relationship_ids_are_reported_once() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="../slideLayouts/a.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="../slideLayouts/b.xml"/>
</Relationships>"#,
            ),
        ]);
        assert_eq!(
            messages(&index)
                .iter()
                .filter(|message| message.contains("declared 2 times"))
                .count(),
            1
        );
    }

    #[test]
    fn missing_required_parts_are_reported() {
        let index = package(&[("ppt/presentation.xml", "<p/>")]);
        let messages = messages(&index);
        assert!(messages.iter().any(|m| m.contains("[Content_Types].xml")));
        assert!(messages.iter().any(|m| m.contains("/_rels/.rels")));
    }

    #[test]
    fn reserved_parts_are_exempt_from_content_type_and_orphan_rules() {
        let index = package(&[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
            ("[trash]/0000.dat", "x"),
        ]);
        assert!(index.integrity.is_empty(), "{:?}", messages(&index));
    }

    #[test]
    fn relationship_parts_are_still_required_to_have_a_content_type() {
        // `.rels` parts are exempt from reachability, not from the content-type
        // rule: a manifest without a `rels` default leaves them untyped.
        let content_types = CONTENT_TYPES.replace(
            "  <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n",
            "",
        );
        let index = package(&[
            ("[Content_Types].xml", &content_types),
            ("_rels/.rels", ROOT_RELS),
            ("ppt/presentation.xml", "<p/>"),
        ]);
        assert_eq!(
            part_of(&index, "no content type").as_deref(),
            Some("/_rels/.rels")
        );
    }

    #[test]
    fn relationship_part_paths_round_trip_to_their_source() {
        assert_eq!(rels_part("/"), ROOT_RELS_PART);
        assert_eq!(
            rels_part("/ppt/presentation.xml"),
            "/ppt/_rels/presentation.xml.rels"
        );
    }
}
