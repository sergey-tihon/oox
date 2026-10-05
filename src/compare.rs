//! Part-level comparison between two OOXML packages.
//!
//! Comparison is content-aware rather than byte-aware: two parts whose bytes
//! differ only by insignificant XML whitespace or attribute order compare
//! equal, so a file that was merely re-saved does not light up the whole tree
//! as changed. Identical bytes are detected from the ZIP central directory
//! (size + CRC32) without decompressing anything.
use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
    io::{self, Read, Seek},
};

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};

use crate::{
    package::{Diagnostic, MAX_ENTRY_BYTES, PackageIndex, PartInfo, PartKind},
    preview::{MAX_XML_PREVIEW_BYTES, Preview, PreviewKind},
};

/// Unified diff output is bounded so an extreme pair of parts cannot balloon
/// the content pane.
pub const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
/// Canonical indentation stops growing past this depth, so deeply nested XML
/// cannot amplify a small part into a huge comparison string.
const MAX_INDENT_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartStatus {
    Added,
    Removed,
    Changed,
    Unchanged,
}

impl PartStatus {
    /// Single-character tree marker.
    pub fn marker(self) -> &'static str {
        match self {
            PartStatus::Added => "+",
            PartStatus::Removed => "-",
            PartStatus::Changed => "~",
            PartStatus::Unchanged => "=",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PartStatus::Added => "Added",
            PartStatus::Removed => "Removed",
            PartStatus::Changed => "Changed",
            PartStatus::Unchanged => "Unchanged",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// Status keyed by package path, including unchanged parts.
    pub statuses: BTreeMap<String, PartStatus>,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// Parts that could not be read or normalized during comparison.
    pub diagnostics: Vec<Diagnostic>,
}

impl Comparison {
    pub fn status_of(&self, path: &str) -> Option<PartStatus> {
        self.statuses.get(path).copied()
    }

    /// Every part of a package compared against itself.
    pub fn identical(index: &PackageIndex) -> Self {
        let mut comparison = Self::default();
        for (path, part) in &index.parts {
            if part.kind != PartKind::Directory {
                comparison.record(path, PartStatus::Unchanged);
            }
        }
        comparison
    }

    pub fn summary_line(&self) -> String {
        format!(
            "Comparison: {} added, {} removed, {} changed, {} unchanged",
            self.added, self.removed, self.changed, self.unchanged
        )
    }

    fn record(&mut self, path: &str, status: PartStatus) {
        match status {
            PartStatus::Added => self.added += 1,
            PartStatus::Removed => self.removed += 1,
            PartStatus::Changed => self.changed += 1,
            PartStatus::Unchanged => self.unchanged += 1,
        }
        self.statuses.insert(path.to_string(), status);
    }
}

/// Compare every part of two indexed packages. Per-part problems become
/// diagnostics and the part is reported as changed rather than aborting the
/// comparison.
pub fn compare<R: Read + Seek, S: Read + Seek>(
    index_a: &PackageIndex,
    archive_a: &mut zip::ZipArchive<R>,
    index_b: &PackageIndex,
    archive_b: &mut zip::ZipArchive<S>,
) -> Comparison {
    let mut comparison = Comparison::default();
    for (path, part_b) in &index_b.parts {
        if part_b.kind == PartKind::Directory {
            continue;
        }
        let status = match index_a.parts.get(path) {
            Some(part_a) if part_a.kind != PartKind::Directory => part_status(
                index_a,
                archive_a,
                part_a,
                index_b,
                archive_b,
                part_b,
                &mut comparison,
            ),
            _ => PartStatus::Added,
        };
        comparison.record(path, status);
    }
    for (path, part_a) in &index_a.parts {
        if part_a.kind == PartKind::Directory
            || index_b
                .parts
                .get(path)
                .is_some_and(|part_b| part_b.kind != PartKind::Directory)
        {
            continue;
        }
        comparison.record(path, PartStatus::Removed);
    }
    comparison
}

fn part_status<R: Read + Seek, S: Read + Seek>(
    index_a: &PackageIndex,
    archive_a: &mut zip::ZipArchive<R>,
    part_a: &PartInfo,
    index_b: &PackageIndex,
    archive_b: &mut zip::ZipArchive<S>,
    part_b: &PartInfo,
    comparison: &mut Comparison,
) -> PartStatus {
    if part_a.size == part_b.size && part_a.crc32 == part_b.crc32 {
        return PartStatus::Unchanged;
    }
    // Only XML can differ in bytes yet match in content; everything else is
    // already known to differ, so it is never decompressed here.
    if part_a.kind != PartKind::Xml || part_b.kind != PartKind::Xml {
        return PartStatus::Changed;
    }
    let (bytes_a, bytes_b) = match (
        read(index_a, archive_a, part_a),
        read(index_b, archive_b, part_b),
    ) {
        (Ok(bytes_a), Ok(bytes_b)) => (bytes_a, bytes_b),
        _ => {
            comparison.diagnostics.push(Diagnostic::warning(
                "compare",
                Some(part_a.path.clone()),
                "part could not be read; reporting a difference",
            ));
            return PartStatus::Changed;
        }
    };
    match (canonical_text(&bytes_a), canonical_text(&bytes_b)) {
        (Ok(text_a), Ok(text_b)) if text_a == text_b => PartStatus::Unchanged,
        (Err(_), _) | (_, Err(_)) => {
            comparison.diagnostics.push(Diagnostic::warning(
                "compare",
                Some(part_a.path.clone()),
                "XML could not be normalized; reporting a difference",
            ));
            PartStatus::Changed
        }
        _ => PartStatus::Changed,
    }
}

/// Build the content pane view for one part: a unified diff of normalized XML,
/// or a size summary for binary parts that cannot be diffed line by line.
pub fn diff_part<R: Read + Seek, S: Read + Seek>(
    index_a: &PackageIndex,
    archive_a: &mut zip::ZipArchive<R>,
    index_b: &PackageIndex,
    archive_b: &mut zip::ZipArchive<S>,
    path: &str,
) -> io::Result<Preview> {
    let part_a = file_part(index_a, path);
    let part_b = file_part(index_b, path);
    let is_xml = part_a
        .into_iter()
        .chain(part_b)
        .any(|part| part.kind == PartKind::Xml);
    if !is_xml {
        // The ZIP central directory already distinguishes byte-identical parts
        // from differing ones, so binary parts are never decompressed here.
        let identical = matches!((part_a, part_b), (Some(a), Some(b)) if a.size == b.size && a.crc32 == b.crc32);
        return Ok(if identical {
            no_differences(path)
        } else {
            binary_diff(path, part_a, part_b)
        });
    }

    let bytes_a = part_a
        .map(|part| read(index_a, archive_a, part))
        .transpose()?;
    let bytes_b = part_b
        .map(|part| read(index_b, archive_b, part))
        .transpose()?;
    let old = bytes_a
        .as_deref()
        .map(canonical_text)
        .transpose()?
        .unwrap_or_default();
    let new = bytes_b
        .as_deref()
        .map(canonical_text)
        .transpose()?
        .unwrap_or_default();
    if old == new {
        return Ok(no_differences(path));
    }
    let text = unified_diff(&old, &new, &label(index_a), &label(index_b), path);
    Ok(Preview::Editor {
        kind: PreviewKind::Diff,
        text,
        editable: false,
    })
}

fn no_differences(path: &str) -> Preview {
    Preview::Editor {
        kind: PreviewKind::Diff,
        text: format!("No differences in {path}\n"),
        editable: false,
    }
}

fn file_part<'a>(index: &'a PackageIndex, path: &str) -> Option<&'a PartInfo> {
    index
        .parts
        .get(path)
        .filter(|part| part.kind != PartKind::Directory)
}

fn read<R: Read + Seek>(
    index: &PackageIndex,
    archive: &mut zip::ZipArchive<R>,
    part: &PartInfo,
) -> io::Result<Vec<u8>> {
    index.read_part(archive, &part.path, MAX_ENTRY_BYTES)
}

fn binary_diff(path: &str, part_a: Option<&PartInfo>, part_b: Option<&PartInfo>) -> Preview {
    let side = |label: &str, part: Option<&PartInfo>| match part {
        Some(part) => format!(
            "{label}: {} bytes ({} compressed)",
            part.size, part.compressed_size
        ),
        None => format!("{label}: absent"),
    };
    let status = match (part_a, part_b) {
        (Some(_), Some(_)) => "Binary part differs",
        (None, Some(_)) => "Binary part added",
        (Some(_), None) => "Binary part removed",
        (None, None) => "Part not found",
    };
    Preview::Info(format!(
        "{status}\n\nPart: {path}\n{}\n{}\n\nNon-XML parts are compared by content, not rendered as a text diff.",
        side("A", part_a),
        side("B", part_b),
    ))
}

fn label(index: &PackageIndex) -> String {
    index
        .source
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "A".to_string())
}

fn unified_diff(old: &str, new: &str, label_a: &str, label_b: &str, path: &str) -> String {
    if old == new {
        return format!("No differences in {path}\n");
    }
    let mut output = BoundedText::new(MAX_DIFF_BYTES);
    let (a, b) = (format!("{label_a}: {path}"), format!("{label_b}: {path}"));
    let diff = similar::TextDiff::from_lines(old, new);
    let _ = write!(
        output,
        "{}",
        diff.unified_diff().context_radius(3).header(&a, &b)
    );
    output.finish()
}

/// Prefer the canonical form; fall back to raw text for malformed XML with valid UTF-8.
/// Invalid UTF-8 is rejected to avoid lossy-decoding collisions.
fn canonical_text(bytes: &[u8]) -> io::Result<String> {
    std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match canonical_xml(bytes) {
        Ok(text) => Ok(text),
        Err(_) if bytes.len() <= MAX_XML_PREVIEW_BYTES => {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
        Err(error) => Err(error),
    }
}

/// Canonical, pretty-printed XML used for comparison: attributes sorted by name,
/// insignificant whitespace dropped. Two parts that differ only by attribute
/// order, spacing, or indentation canonicalize identically.
pub fn canonical_xml(bytes: &[u8]) -> io::Result<String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut output = String::new();
    let mut buffer = Vec::new();
    let mut depth = 0usize;
    let mut pending = String::new();
    let mut elements = Vec::new();
    let mut formatting_whitespace = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                let inherited = elements
                    .last()
                    .is_some_and(|element: &ElementContext| element.preserve_space);
                if let Some(parent) = elements.last_mut() {
                    parent.has_child_elements = true;
                }
                flush_text(&mut output, &mut pending, depth, elements.last_mut());
                elements.push(ElementContext {
                    preserve_space: xml_space_preserved(&event, inherited),
                    ..ElementContext::default()
                });
                write_tag(&mut output, depth, &event, false);
                depth += 1;
            }
            Ok(Event::Empty(event)) => {
                if let Some(parent) = elements.last_mut() {
                    parent.has_child_elements = true;
                }
                flush_text(&mut output, &mut pending, depth, elements.last_mut());
                write_tag(&mut output, depth, &event, true);
            }
            Ok(Event::End(event)) => {
                flush_text(&mut output, &mut pending, depth, elements.last_mut());
                if let Some(element) = elements.pop() {
                    if element.has_child_elements
                        && !element.has_non_whitespace_text
                        && !element.preserve_space
                    {
                        formatting_whitespace.extend(element.whitespace_ranges);
                    }
                }
                depth = depth.saturating_sub(1);
                push_indent(&mut output, depth);
                output.push_str("</");
                output.push_str(event.name().as_ref());
                output.push_str(">\n");
            }
            Ok(Event::Text(event)) => append_pending(&mut pending, event.as_ref())?,
            Ok(Event::CData(event)) => append_cdata(&mut pending, event.as_ref())?,
            Ok(Event::GeneralRef(event)) => {
                append_pending(&mut pending, "&")?;
                append_pending(&mut pending, event.as_ref())?;
                append_pending(&mut pending, ";")?;
            }
            Ok(Event::Comment(event)) => {
                flush_text(&mut output, &mut pending, depth, elements.last_mut());
                push_indent(&mut output, depth);
                output.push_str("<!--");
                output.push_str(event.as_ref().trim());
                output.push_str("-->\n");
            }
            Ok(Event::PI(event)) => {
                flush_text(&mut output, &mut pending, depth, elements.last_mut());
                push_indent(&mut output, depth);
                output.push_str("<?");
                output.push_str(event.as_ref());
                output.push_str("?>\n");
            }
            // Declarations and doctypes are structural noise for a content diff.
            Ok(_) => {}
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        }
        buffer.clear();
        if output.len() > MAX_XML_PREVIEW_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("canonical XML exceeds {MAX_XML_PREVIEW_BYTES} byte limit"),
            ));
        }
    }
    flush_text(&mut output, &mut pending, depth, elements.last_mut());
    formatting_whitespace.sort_unstable_by_key(|(start, _)| std::cmp::Reverse(*start));
    for (start, end) in formatting_whitespace {
        output.replace_range(start..end, "");
    }
    if output.len() > MAX_XML_PREVIEW_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("canonical XML exceeds {MAX_XML_PREVIEW_BYTES} byte limit"),
        ));
    }
    Ok(output)
}

fn append_pending(pending: &mut String, text: &str) -> io::Result<()> {
    if pending.len().saturating_add(text.len()) > MAX_XML_PREVIEW_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("canonical XML text exceeds {MAX_XML_PREVIEW_BYTES} byte limit"),
        ));
    }
    pending.push_str(text);
    Ok(())
}

// ponytail: schema-less heuristic treats whitespace-only text in child-bearing,
// text-free elements as formatting; use schema content models for exact handling.
#[derive(Default)]
struct ElementContext {
    preserve_space: bool,
    has_child_elements: bool,
    has_non_whitespace_text: bool,
    whitespace_ranges: Vec<(usize, usize)>,
}

fn append_cdata(pending: &mut String, text: &str) -> io::Result<()> {
    let mut start = 0;
    for (index, character) in text.char_indices() {
        let escaped = match character {
            '&' => "&amp;",
            '<' => "&lt;",
            _ => continue,
        };
        append_pending(pending, &text[start..index])?;
        append_pending(pending, escaped)?;
        start = index + character.len_utf8();
    }
    append_pending(pending, &text[start..])
}

fn xml_space_preserved(event: &BytesStart<'_>, inherited: bool) -> bool {
    event
        .attributes()
        .with_checks(false)
        .filter_map(Result::ok)
        .find(|attribute| attribute.key.as_ref() == "xml:space")
        .map(|attribute| match attribute.value.as_ref() {
            "preserve" => true,
            "default" => false,
            _ => inherited,
        })
        .unwrap_or(inherited)
}

fn write_tag(output: &mut String, depth: usize, event: &BytesStart<'_>, empty: bool) {
    let mut attributes: Vec<(String, String)> = event
        .attributes()
        .with_checks(false)
        .filter_map(Result::ok)
        .map(|attribute| {
            (
                attribute.key.as_ref().to_string(),
                attribute.value.into_owned(),
            )
        })
        .collect();
    attributes.sort();

    push_indent(output, depth);
    output.push('<');
    output.push_str(event.name().as_ref());
    for (key, value) in attributes {
        output.push(' ');
        output.push_str(&key);
        output.push_str("=\"");
        output.push_str(&value.replace('"', "&quot;"));
        output.push('"');
    }
    output.push_str(if empty { "/>\n" } else { ">\n" });
}

fn flush_text(
    output: &mut String,
    pending: &mut String,
    depth: usize,
    mut element: Option<&mut ElementContext>,
) {
    if pending.is_empty() {
        return;
    }
    let whitespace_only = pending.chars().all(char::is_whitespace);
    if whitespace_only && element.is_none() {
        pending.clear();
        return;
    }
    if !whitespace_only {
        if let Some(element) = element.as_deref_mut() {
            element.has_non_whitespace_text = true;
        }
    }
    let start = output.len();
    push_indent(output, depth);
    output.push_str(pending);
    output.push('\n');
    if whitespace_only {
        if let Some(element) = element {
            element.whitespace_ranges.push((start, output.len()));
        }
    }
    pending.clear();
}

fn push_indent(output: &mut String, depth: usize) {
    for _ in 0..depth.min(MAX_INDENT_DEPTH) {
        output.push_str("  ");
    }
}

/// A `fmt::Write` sink that silently drops output past `limit` and records that
/// it did, so diff formatting cannot exceed a fixed budget.
struct BoundedText {
    text: String,
    limit: usize,
    truncated: bool,
}

impl BoundedText {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            truncated: false,
        }
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.text.push_str("\n[diff truncated]\n");
        }
        self.text
    }
}

impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() <= self.limit.saturating_sub(self.text.len()) {
            self.text.push_str(value);
            return Ok(());
        }
        self.truncated = true;
        let remaining = self.limit.saturating_sub(self.text.len());
        let end = (0..=remaining.min(value.len()))
            .rev()
            .find(|index| value.is_char_boundary(*index))
            .unwrap_or(0);
        self.text.push_str(&value[..end]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write as _};

    fn test_archive(
        entries: &[(&str, Option<&[u8]>)],
    ) -> io::Result<zip::ZipArchive<Cursor<Vec<u8>>>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            let options = zip::write::SimpleFileOptions::default();
            if let Some(content) = content {
                writer
                    .start_file(*name, options)
                    .map_err(io::Error::other)?;
                writer.write_all(content)?;
            } else {
                writer
                    .add_directory(*name, options)
                    .map_err(io::Error::other)?;
            }
        }
        zip::ZipArchive::new(writer.finish().map_err(io::Error::other)?).map_err(io::Error::other)
    }

    #[test]
    fn file_replaced_by_directory_is_reported_as_removed() -> io::Result<()> {
        let mut archive_a = test_archive(&[("foo", Some(b"old"))])?;
        let mut archive_b = test_archive(&[("foo/", None), ("foo/child.xml", Some(b"<child/>"))])?;
        let index_a = PackageIndex::from_archive(&mut archive_a)?;
        let index_b = PackageIndex::from_archive(&mut archive_b)?;

        let comparison = compare(&index_a, &mut archive_a, &index_b, &mut archive_b);
        assert_eq!(comparison.status_of("/foo"), Some(PartStatus::Removed));
        assert_eq!(
            comparison.status_of("/foo/child.xml"),
            Some(PartStatus::Added)
        );
        Ok(())
    }

    #[test]
    fn canonical_xml_ignores_attribute_order_and_formatting_whitespace() {
        let a = canonical_xml(br#"<root b="2" a="1"><item>hi there</item></root>"#).unwrap();
        let b = canonical_xml(
            br#"<root
                 a="1"   b="2">
                  <item>hi there</item></root>"#,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a,
            "<root a=\"1\" b=\"2\">\n  <item>\n    hi there\n  </item>\n</root>\n"
        );
    }

    #[test]
    fn canonical_xml_escapes_attribute_quotes_when_changing_delimiters() {
        let quoted = canonical_xml(br#"<root a='x" y="z'/>"#).unwrap();
        let separate = canonical_xml(br#"<root a="x" y="z"/>"#).unwrap();
        assert!(quoted.contains("&quot;"));
        assert_ne!(quoted, separate);
    }

    #[test]
    fn oversized_xml_is_rejected_instead_of_falling_back_to_raw_text() {
        let mut xml = Vec::with_capacity(MAX_XML_PREVIEW_BYTES + 16);
        xml.extend_from_slice(b"<root>");
        xml.resize(MAX_XML_PREVIEW_BYTES + 8, b'x');
        xml.extend_from_slice(b"</root>");
        assert!(canonical_xml(&xml).is_err());
        assert!(canonical_text(&xml).is_err());
    }

    #[test]
    fn invalid_utf8_xml_is_reported_as_changed() -> io::Result<()> {
        let mut archive_a = test_archive(&[("bad.xml", Some(b"<root>\xff</root>"))])?;
        let mut archive_b = test_archive(&[("bad.xml", Some(b"<root>\xfe</root>"))])?;
        let index_a = PackageIndex::from_archive(&mut archive_a)?;
        let index_b = PackageIndex::from_archive(&mut archive_b)?;

        let comparison = compare(&index_a, &mut archive_a, &index_b, &mut archive_b);
        assert_eq!(comparison.status_of("/bad.xml"), Some(PartStatus::Changed));
        assert_eq!(comparison.diagnostics.len(), 1);
        Ok(())
    }

    #[test]
    fn canonical_xml_keeps_cdata_as_text_not_markup() {
        let cdata = canonical_xml(b"<root><![CDATA[<x/>]]></root>").unwrap();
        let escaped_text = canonical_xml(b"<root>&lt;x/></root>").unwrap();
        let child_element = canonical_xml(b"<root><x/></root>").unwrap();
        assert_eq!(cdata, escaped_text);
        assert_ne!(cdata, child_element);
    }

    #[test]
    fn canonical_xml_preserves_significant_text_whitespace() {
        let whitespace_leaf = canonical_xml(b"<t>\n</t>").unwrap();
        let empty_leaf = canonical_xml(b"<t></t>").unwrap();
        assert_ne!(whitespace_leaf, empty_leaf);

        let formatted = canonical_xml(b"<root> \t<a/>\n\t<b/> \t</root>").unwrap();
        let compact = canonical_xml(b"<root><a/><b/></root>").unwrap();
        assert_eq!(formatted, compact);

        let leading_mixed_space = canonical_xml(b"<root> <b/>text</root>").unwrap();
        let no_leading_mixed_space = canonical_xml(b"<root><b/>text</root>").unwrap();
        assert_ne!(leading_mixed_space, no_leading_mixed_space);

        let single_space = canonical_xml(b"<root><t>hello world</t></root>").unwrap();
        let repeated_space = canonical_xml(b"<root><t>hello  world</t></root>").unwrap();
        assert_ne!(single_space, repeated_space);

        let mixed_space = canonical_xml(b"<root>hello <b/> world</root>").unwrap();
        let mixed_no_space = canonical_xml(b"<root>hello<b/>world</root>").unwrap();
        assert_ne!(mixed_space, mixed_no_space);

        let preserved = canonical_xml(b"<root xml:space=\"preserve\">\n </root>").unwrap();
        let empty = canonical_xml(b"<root xml:space=\"preserve\"></root>").unwrap();
        assert_ne!(preserved, empty);
    }

    #[test]
    fn canonical_xml_keeps_real_changes_apart() {
        let a = canonical_xml(br#"<root a="1"/>"#).unwrap();
        let b = canonical_xml(br#"<root a="2"/>"#).unwrap();
        assert_ne!(a, b);
        assert!(canonical_xml(b"<root><item></root>").is_err());
    }

    #[test]
    fn diff_text_marks_added_lines() {
        let diff = unified_diff(
            "<root/>\n",
            "<root>\n  <slide/>\n</root>\n",
            "before.pptx",
            "after.pptx",
            "/ppt/presentation.xml",
        );
        assert!(diff.contains("+  <slide/>"), "unexpected diff: {diff}");
        assert!(diff.contains("before.pptx: /ppt/presentation.xml"));
        assert_eq!(
            unified_diff("same\n", "same\n", "a", "b", "/p"),
            "No differences in /p\n"
        );
    }

    #[test]
    fn bounded_text_truncates_at_a_char_boundary() {
        let mut output = BoundedText::new(4);
        let _ = write!(output, "abé-cdef");
        let text = output.finish();
        assert!(text.starts_with("abé"));
        assert!(text.contains("[diff truncated]"));
    }
}
