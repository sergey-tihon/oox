use super::edit::{lines_to_text, pick_up_external};
use super::tree::{create_tree, styled_tree_label, tree_label};
use super::{CursorSpot, PendingExport, place_cursor};
use crate::compare::PartStatus;
use crate::preview::PreviewKind;
use crate::{App, worker::Worker};
use ratatui_image::picker::Picker;
use std::{io, path::PathBuf, sync::Arc, time::Duration};

/// Pump the worker until `done` holds, with a generous timeout. Tests run the
/// real worker thread; they only avoid fixed sleeps.
fn pump_until(app: &mut App, done: impl Fn(&App) -> bool) {
    for _ in 0..1_000 {
        if done(app) {
            return;
        }
        app.poll_worker();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(done(app), "timed out waiting for background work");
}

fn test_app(path: &str) -> io::Result<App> {
    let worker = Worker::start()?;
    let mut app = App::new_loading(path.to_string(), None, Picker::halfblocks(), worker)?;
    pump_until(&mut app, |app| !app.loading);
    Ok(app)
}

fn preview_loaded(app: &mut App) {
    pump_until(app, |app| !app.preview.pending);
}

#[test]
fn yank_survives_switching_parts() -> io::Result<()> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let key = |character| Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    let second_row = |app: &App| {
        lines_to_text(&app.editor.state.lines)
            .lines()
            .nth(1)
            .unwrap_or_default()
            .to_string()
    };
    let mut app = test_app("data/sample.pptx")?;
    let mut handler = edtui::EditorEventHandler::vim_mode();

    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    handler.on_event(key('j'), &mut app.editor.state);
    let yanked = second_row(&app);
    assert!(!yanked.is_empty());
    handler.on_event(key('y'), &mut app.editor.state);
    handler.on_event(key('y'), &mut app.editor.state);

    app.tree
        .state
        .select(vec!["/ppt/presentation.xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert_ne!(second_row(&app), yanked);
    handler.on_event(key('p'), &mut app.editor.state);
    let text = lines_to_text(&app.editor.state.lines);
    assert_eq!(text.lines().nth(1), Some(yanked.as_str()));
    Ok(())
}
#[test]
fn details_cursor_survives_selection_with_fewer_links() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/ppt/presentation.xml".to_string()]);
    let links_len = app.details_view().links.len();
    assert!(links_len > 0);

    app.details.cursor = 50;
    app.move_details_cursor(true);

    assert!(app.details.cursor < links_len);
    Ok(())
}

#[test]
fn loading_constructor_installs_worker_package_result() -> io::Result<()> {
    let worker = Worker::start()?;
    let mut app = App::new_loading(
        "data/sample.pptx".to_string(),
        None,
        Picker::halfblocks(),
        worker,
    )?;
    assert!(app.loading);
    assert!(app.tree.items.is_empty());
    pump_until(&mut app, |app| !app.loading);
    assert!(app.is_package_loaded());
    assert!(!app.tree.items.is_empty());
    assert!(app.summary.view.is_some());
    Ok(())
}

#[test]
fn loading_error_keeps_no_package_state() -> io::Result<()> {
    let mut app = test_app("/definitely/not/a/package.pptx")?;
    assert!(!app.is_package_loaded());
    assert!(app.tree.items.is_empty());
    assert!(app.selection_status().contains("No package"));
    app.expand_all();
    app.collapse_all();
    Ok(())
}

#[test]
fn load_pptx() -> io::Result<()> {
    let app = test_app("data/sample.pptx")?;
    assert!(!app.tree.items.is_empty());
    let summary = app
        .summary
        .view
        .as_ref()
        .expect("sample presentation should have a summary");
    assert!(summary.text.contains("Slides: 2"));
    assert!(summary.text.contains("OOXML TUI"));
    assert!(
        summary
            .links
            .iter()
            .any(|link| link.target == "/ppt/slides/slide1.xml")
    );

    Ok(())
}

#[test]
fn summary_links_navigate_to_package_parts() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.toggle_summary()?;
    let link = app
        .summary
        .view
        .as_ref()
        .and_then(|summary| {
            summary
                .links
                .iter()
                .find(|link| link.target == "/ppt/slides/slide1.xml")
        })
        .cloned()
        .expect("summary should link to the first slide");
    assert!(app.activate_summary_link(link.line, link.start)?);
    assert!(!app.summary.visible);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/slides/slide1.xml")
    );
    preview_loaded(&mut app);
    assert_eq!(app.preview.kind, PreviewKind::Xml);
    Ok(())
}

#[test]
fn toggle_document_summary_switches_back_to_selected_content() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.toggle_summary()?;
    assert_eq!(app.preview.kind, PreviewKind::Summary);
    assert!(app.summary.visible);

    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.toggle_summary()?;
    assert!(!app.summary.visible);
    preview_loaded(&mut app);
    assert_eq!(app.preview.kind, PreviewKind::Xml);
    assert!(app.content_message.is_none());
    Ok(())
}

#[test]
fn load_selected_file_content() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    assert!(app.details.visible);
    app.tree
        .state
        .select(vec!["/ppt/media/image1.gif".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert!(app.preview.image_state.is_some());
    assert!(app.content_message.is_none());

    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert!(app.preview.image_state.is_none());
    assert!(app.content_message.is_none());

    app.tree.state.select(vec!["/ppt".to_string()]);
    app.load_selected_file_content()?;
    assert!(app.preview.image_state.is_none());
    assert_eq!(app.content_message.as_deref(), Some("Directory: ppt"));

    Ok(())
}

#[test]
fn expand_and_collapse_all_tree_nodes() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.expand_all();
    assert!(!app.tree.state.opened().is_empty());

    app.collapse_all();
    assert!(app.tree.state.opened().is_empty());
    assert_eq!(app.tree.state.selected().len(), 1);

    Ok(())
}

#[test]
fn navigation_history_moves_back_and_forward() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    app.tree.state.select(vec![
        "/ppt".to_string(),
        "/ppt/presentation.xml".to_string(),
    ]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);

    assert!(app.navigate_back()?);
    preview_loaded(&mut app);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/[Content_Types].xml")
    );
    assert!(app.navigate_forward()?);
    preview_loaded(&mut app);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/presentation.xml")
    );

    Ok(())
}

#[test]
fn navigation_history_is_capped() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    for index in 0..(super::navigation::MAX_NAVIGATION_HISTORY + 50) {
        app.record_navigation(&format!("/part-{index}.xml"));
    }
    assert!(app.history.back.len() <= super::navigation::MAX_NAVIGATION_HISTORY);
    Ok(())
}

#[test]
fn details_view_is_cached_per_selection() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    let first = app.details_view().text.clone();
    let second = app.details_view().text.clone();
    assert_eq!(first, second);
    assert!(first.contains("[Content_Types].xml"));

    app.tree.state.select(vec!["/ppt".to_string()]);
    let third = app.details_view().text.clone();
    assert!(third.contains("ppt"));
    assert_ne!(first, third);
    Ok(())
}

#[test]
fn package_metadata_and_relationships_are_indexed() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let slide = app
        .index()
        .parts
        .get("/ppt/slides/slide1.xml")
        .expect("sample slide should be indexed");
    assert_eq!(slide.kind, crate::package::PartKind::Xml);
    assert!(
        slide
            .content_type
            .as_deref()
            .is_some_and(|content_type| { content_type.contains("presentationml.slide+xml") })
    );

    let outgoing = app
        .index()
        .outgoing
        .get("/ppt/slides/slide1.xml")
        .expect("sample slide should have relationships");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        app.index().relationships[outgoing[0]]
            .resolved_target
            .as_deref(),
        Some("/ppt/slideLayouts/slideLayout1.xml")
    );

    app.tree.state.select(vec![
        "/ppt".to_string(),
        "/ppt/slides".to_string(),
        "/ppt/slides/slide1.xml".to_string(),
    ]);
    let (line, start) = {
        let view = app.details_view();
        let link = view.links.first().expect("details should have links");
        (link.line, link.start)
    };
    app.activate_detail_link(line, start)?;
    preview_loaded(&mut app);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/slideLayouts/slideLayout1.xml")
    );

    Ok(())
}

/// Place the editor cursor on the first occurrence of `needle`.
fn put_cursor_on(app: &mut App, needle: &str) -> bool {
    for (row, line) in app.editor.state.lines.to_vecs().iter().enumerate() {
        let text: String = line.iter().collect();
        if let Some(byte_offset) = text.find(needle) {
            let column = text[..byte_offset].chars().count();
            app.editor.state.cursor = edtui::Index2::new(row, column);
            return true;
        }
    }
    false
}

/// Issue #14 acceptance: an `r:embed="rIdN"` token in slide XML opens the
/// referenced image part, `Alt-Left` returns to the slide, and an external
/// hyperlink reference is reported in the status bar instead of followed.
#[test]
fn relationship_references_jump_to_their_targets() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let selected = |app: &App| app.tree.state.selected().last().cloned();
    app.select_path("/ppt/slides/slide2.xml");
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert_eq!(app.preview.kind, PreviewKind::Xml);

    // Ordinary XML at the cursor is not a reference.
    app.editor.state.cursor = edtui::Index2::new(0, 0);
    assert!(!app.follow_relationship_at_cursor()?);
    assert!(app.status_message.is_none());

    assert!(put_cursor_on(&mut app, "\"rId2\""), "slide embeds rId2");
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/media/image1.gif"));
    assert!(app.preview.image_state.is_some());

    assert!(app.navigate_back()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide2.xml"));

    assert!(put_cursor_on(&mut app, "r:id=\"rId3\""));
    assert!(app.follow_relationship_at_cursor()?);
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("https://chunyu.site/neovim/"))
    );
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide2.xml"));

    // The tree selection may move on without reloading the preview; tokens
    // must still resolve against the part shown in the editor, not the
    // selection. slide1 (the selection) has no rId2, so a selection-based
    // lookup would fail to jump at all.
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    assert!(put_cursor_on(&mut app, "r:embed=\"rId2\""));
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/media/image1.gif"));
    Ok(())
}

/// A package whose slide references two navigable parts and two unusable
/// targets through `r:*` attributes written in the lexical forms XML permits
/// (`pretty_print_xml` forwards the raw start tag, so these reach the
/// preview unchanged).
fn write_reference_package(name: &str) -> io::Result<std::path::PathBuf> {
    use std::io::Write as _;

    let path = std::env::temp_dir().join(format!("oox-test-{}-{name}", std::process::id()));
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let entries = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
</Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#,
        ),
        ("ppt/presentation.xml", "<p:presentation/>"),
        (
            "ppt/slides/slide1.xml",
            "<p:sld xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><a:blip r:embed='rId10'/><a:blip\n    r:id\n    =\n    \"rId11\"/><a:blip r:link=\"rId12\"/><a:blip r:id=\"rId13\"/><a:blip descr=\"A > B and it's fine\" r:embed=\"rId14\"/><a:t>see r:id=\"rId10\" here</a:t></p:sld>",
        ),
        (
            "ppt/slides/_rels/slide1.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slide2.xml"/>
  <Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slide3.xml"/>
  <Relationship Id="rId12" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target=".."/>
  <Relationship Id="rId13" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="missing.xml"/>
  <Relationship Id="rId14" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="../presentation.xml"/>
</Relationships>"#,
        ),
        ("ppt/slides/slide2.xml", "<p:sld/>"),
        ("ppt/slides/slide3.xml", "<p:sld/>"),
    ];
    for (entry, content) in entries {
        writer
            .start_file(entry, zip::write::SimpleFileOptions::default())
            .map_err(io::Error::other)?;
        writer.write_all(content.as_bytes())?;
    }
    writer.finish().map_err(io::Error::other)?;
    Ok(path)
}

/// XML permits single-quoted attribute values and whitespace around `=`, and
/// a start tag may span several preview rows because `pretty_print_xml`
/// forwards the raw tag bytes. All of those forms stay visible in the preview
/// and must be followed, while the same text in element content must not be.
/// A directory or missing target is reported instead of navigating away.
#[test]
fn relationship_reference_lexical_variants_and_unusable_targets() -> io::Result<()> {
    let path = write_reference_package("follow-variants.pptx")?;
    let mut app = test_app(&path.to_string_lossy())?;
    let selected = |app: &App| app.tree.state.selected().last().cloned();
    app.select_path("/ppt/slides/slide1.xml");
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert_eq!(app.preview.kind, PreviewKind::Xml);

    // Single quotes.
    assert!(put_cursor_on(&mut app, "r:embed='rId10'"));
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide2.xml"));
    assert_eq!(app.preview.kind, PreviewKind::Xml);

    assert!(app.navigate_back()?);
    preview_loaded(&mut app);

    // An attribute whose name, `=`, and value sit on three separate preview
    // rows is followed from the name row and from the value row.
    assert!(put_cursor_on(&mut app, "r:id"));
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide3.xml"));

    assert!(app.navigate_back()?);
    preview_loaded(&mut app);
    assert!(put_cursor_on(&mut app, "rId11"));
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide3.xml"));

    assert!(app.navigate_back()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide1.xml"));

    // `Target=".."` resolves to `/ppt`, a directory that holds parts; it is
    // still not a navigable part, so nothing moves and the status explains it.
    assert!(put_cursor_on(&mut app, "rId12"));
    assert!(app.follow_relationship_at_cursor()?);
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("/ppt") && message.contains("is not a part"))
    );
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide1.xml"));
    assert_eq!(app.preview.kind, PreviewKind::Xml);

    assert!(put_cursor_on(&mut app, "rId13"));
    assert!(app.follow_relationship_at_cursor()?);
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("missing.xml"))
    );
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide1.xml"));

    // `descr="A > B and it's fine"` precedes the reference: a `>` and the
    // other quote character inside a value are literal, not tag boundaries.
    assert!(put_cursor_on(&mut app, "r:embed=\"rId14\""));
    assert!(app.follow_relationship_at_cursor()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/presentation.xml"));
    assert!(app.navigate_back()?);
    preview_loaded(&mut app);
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide1.xml"));

    // `<a:t>see r:id="rId10" here</a:t>` only looks like an attribute: it is
    // element content, so the cursor there follows nothing.
    app.status_message = None;
    assert!(put_cursor_on(&mut app, "r:id=\"rId10\""));
    assert!(!app.follow_relationship_at_cursor()?);
    assert!(app.status_message.is_none());
    assert_eq!(selected(&app).as_deref(), Some("/ppt/slides/slide1.xml"));

    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn search_selects_matching_package_parts() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.start_search();
    for character in "/ppt/slides/slide1.xml".chars() {
        app.search_input_char(character);
    }
    app.finish_search();

    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/slides/slide1.xml")
    );
    assert_eq!(app.search.matches.len(), 1);
    assert!(app.selection_status().contains("Type: XML"));

    Ok(())
}

#[test]
fn jumping_to_issues_reports_when_there_are_none() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    assert!(app.index().integrity.is_empty());
    assert!(!app.next_integrity_issue(false));
    assert_eq!(
        app.status_message.as_deref(),
        Some("No package integrity issues")
    );
    assert!(
        app.selection_status()
            .contains("No package integrity issues")
    );
    Ok(())
}

/// A package with a dangling relationship target and a part without a
/// content type. Returns its path so the caller can delete it afterwards.
fn write_test_package(name: &str) -> io::Result<std::path::PathBuf> {
    use std::io::Write as _;

    let path = std::env::temp_dir().join(format!("oox-test-{}-{name}", std::process::id()));
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let entries = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
</Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#,
        ),
        ("ppt/presentation.xml", "<p:presentation/>"),
        (
            "ppt/_rels/presentation.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../p.xml"/>
</Relationships>"#,
        ),
        ("ppt/notes.txt", "notes"),
        ("ppt/p.xml", "<p/>"),
        ("ppt/aaa.txt", "aaa"),
    ];
    for (entry, content) in entries {
        writer
            .start_file(entry, zip::write::SimpleFileOptions::default())
            .map_err(io::Error::other)?;
        writer.write_all(content.as_bytes())?;
    }
    writer.finish().map_err(io::Error::other)?;
    Ok(path)
}

/// Acceptance for issue #12 phase 1: a dangling relationship target is
/// reported, marked in the tree, and reachable from the issue list.
#[test]
fn integrity_issues_mark_the_tree_and_are_navigable() -> io::Result<()> {
    let path = write_test_package("integrity.pptx")?;
    let mut app = test_app(&path.to_string_lossy())?;

    let issues = &app.index().integrity;
    assert!(issues.iter().any(|issue| {
        issue.part.as_deref() == Some("/ppt/presentation.xml")
            && issue.message.contains("/ppt/slides/slide1.xml")
    }));
    assert!(issues.iter().any(|issue| {
        issue.part.as_deref() == Some("/ppt/notes.txt") && issue.message.contains("no content type")
    }));

    // `i` walks the offending parts and clears the stale status message.
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.status_message = Some("old".to_string());
    assert!(app.next_integrity_issue(false));
    assert_eq!(app.status_message, None);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/aaa.txt")
    );

    // The issue list in the metadata panel links to the offending part.
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    let link = app
        .details_view()
        .links
        .iter()
        .find(|link| link.target == "/ppt/presentation.xml")
        .cloned()
        .expect("the issue list should link to the offending part");
    assert!(app.activate_detail_link(link.line, link.start)?);
    assert_eq!(
        app.tree.state.selected().last().map(String::as_str),
        Some("/ppt/presentation.xml")
    );
    preview_loaded(&mut app);

    let markers = app.tree_markers();
    assert_eq!(
        tree_label("presentation.xml", "/ppt/presentation.xml", &markers),
        "presentation.xml ⚠"
    );
    assert_eq!(
        tree_label("presentation.xml", "/ppt/other.xml", &markers),
        "presentation.xml"
    );

    std::fs::remove_file(&path)?;
    Ok(())
}

/// Issue navigation must be directional from any tree position, not just
/// from one of the list ends, and must not leave the destination hidden
/// behind an applied search filter.
#[test]
fn issue_jumps_are_directional_and_clear_the_search_filter() -> io::Result<()> {
    let path = write_test_package("integrity-direction.pptx")?;
    let mut app = test_app(&path.to_string_lossy())?;
    fn selected(app: &App) -> String {
        app.tree
            .state
            .selected()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    // `/ppt/p.xml` has no issue and sorts between two that do.
    assert!(app.index().parts.contains_key("/ppt/p.xml"));
    app.select_path("/ppt/p.xml");
    assert!(app.next_integrity_issue(false));
    assert_eq!(selected(&app), "/ppt/presentation.xml");

    app.select_path("/ppt/p.xml");
    assert!(app.next_integrity_issue(true));
    assert_eq!(selected(&app), "/ppt/notes.txt");

    // Wrap around at the ends.
    app.select_path("/ppt/presentation.xml");
    assert!(app.next_integrity_issue(false));
    assert_eq!(selected(&app), "/ppt/aaa.txt");
    app.select_path("/ppt/aaa.txt");
    assert!(app.next_integrity_issue(true));
    assert_eq!(selected(&app), "/ppt/presentation.xml");

    // A filter that hides the next issue is dropped so the jump is visible.
    app.start_search();
    for character in "aaa.txt".chars() {
        app.search_input_char(character);
    }
    app.finish_search();
    assert!(app.tree_filter_active());
    assert!(app.next_integrity_issue(false));
    assert!(!app.tree_filter_active());
    assert_eq!(selected(&app), "/ppt/notes.txt");
    assert!(flatten_identifiers(app.visible_tree_items()).contains(&"/ppt/notes.txt".to_string()));

    // Activating a link does the same for its destination.
    app.start_search();
    for character in "aaa.txt".chars() {
        app.search_input_char(character);
    }
    app.finish_search();
    app.select_path("/ppt/aaa.txt");
    let link = app
        .details_view()
        .links
        .iter()
        .find(|link| link.target == "/ppt/notes.txt")
        .cloned()
        .expect("the issue list should link to the other issue part");
    assert!(app.activate_detail_link(link.line, link.start)?);
    assert!(!app.tree_filter_active());
    assert_eq!(selected(&app), "/ppt/notes.txt");
    assert!(flatten_identifiers(app.visible_tree_items()).contains(&"/ppt/notes.txt".to_string()));
    preview_loaded(&mut app);

    std::fs::remove_file(&path)?;
    Ok(())
}

fn flatten_identifiers(items: &[tui_tree_widget::TreeItem<'static, String>]) -> Vec<String> {
    let mut result = Vec::new();
    for item in items {
        result.push(item.identifier().clone());
        result.extend(flatten_identifiers(item.children()));
    }
    result
}
#[test]
fn narrowing_filter_equals_fresh_filter() -> io::Result<()> {
    let paths = [
        "ppt/slides/slide1.xml".to_string(),
        "ppt/slides/slide2.xml".to_string(),
        "ppt/media/image1.png".to_string(),
        "word/document.xml".to_string(),
    ];
    let items = create_tree(&paths, &Default::default())?;
    let narrowed = super::filter_tree(&items, "sl")?;
    let narrowed = super::filter_tree(&narrowed, "slide2")?;
    let fresh = super::filter_tree(&items, "slide2")?;

    assert_eq!(flatten_identifiers(&narrowed), flatten_identifiers(&fresh));
    Ok(())
}

#[test]
fn search_filters_tree_live_while_typing() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let full_tree = flatten_identifiers(&app.tree.items);
    assert!(full_tree.contains(&"/docProps/core.xml".to_string()));

    app.start_search();
    for character in "slide1.xml".chars() {
        app.search_input_char(character);
    }

    // Live, before Enter: non-matching paths are hidden, ancestors of
    // matches are retained, and the selection already follows the filter.
    let visible = flatten_identifiers(app.visible_tree_items());
    assert!(visible.contains(&"/ppt/slides/slide1.xml".to_string()));
    assert!(visible.contains(&"/ppt".to_string()));
    assert!(visible.contains(&"/ppt/slides".to_string()));
    assert!(!visible.contains(&"/docProps/core.xml".to_string()));
    assert!(visible.len() < full_tree.len());
    // Selection follows the first sorted match (the slide's .rels sorts first).
    let first_match = app.search.matches.first().cloned();
    assert_eq!(
        app.tree.state.selected().last().cloned().as_ref(),
        first_match.as_ref()
    );
    assert!(app.tree_filter_active());

    // Backspacing to an empty query restores the full tree view.
    for _ in 0.."slide1.xml".chars().count() {
        app.search_backspace();
    }
    assert!(!app.tree_filter_active());
    assert_eq!(flatten_identifiers(app.visible_tree_items()), full_tree);
    Ok(())
}

#[test]
fn finish_search_keeps_filter_and_cancel_restores_tree_state() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let opened_before: Vec<Vec<String>> = app.tree.state.opened().iter().cloned().collect();

    app.start_search();
    for character in "slideLayouts".chars() {
        app.search_input_char(character);
    }
    app.finish_search();

    // The filter stays applied after Enter so n/N can cycle the matches.
    assert!(!app.search.active);
    assert!(app.tree_filter_active());
    // Every retained path either matches the query or is an ancestor of a match.
    let visible = flatten_identifiers(app.visible_tree_items());
    assert!(!visible.is_empty());
    for path in &visible {
        let is_ancestor_of_match = visible
            .iter()
            .any(|other| other.starts_with(&format!("{path}/")));
        assert!(
            path.contains("slideLayouts") || is_ancestor_of_match,
            "unexpected path in filtered tree: {path}"
        );
    }

    // Esc (cancel) restores the full tree and the pre-search open state.
    app.cancel_search();
    assert!(!app.tree_filter_active());
    let opened_after: Vec<Vec<String>> = app.tree.state.opened().iter().cloned().collect();
    assert_eq!(opened_before.len(), opened_after.len());
    assert!(app.search.query.is_empty());
    Ok(())
}
#[test]
fn content_search_filter_shows_edit_marker() -> io::Result<()> {
    fn visible_label(
        items: &[tui_tree_widget::TreeItem<'static, String>],
        path: &str,
    ) -> Option<String> {
        for item in items {
            if item.identifier() == path {
                return Some(format!("{item:?}"));
            }
            if let Some(label) = visible_label(item.children(), path) {
                return Some(label);
            }
        }
        None
    }

    let mut app = test_app("data/sample.pptx")?;
    app.start_content_search();
    for character in "OOXML TUI".chars() {
        app.content_search_input_char(character);
    }
    pump_until(&mut app, |app| !app.content_search.pending);

    let path = "/ppt/slides/slide1.xml";
    assert!(
        app.content_search
            .matches
            .iter()
            .any(|match_path| match_path == path)
    );
    edit_part(&mut app, path, "<p:sld/>")?;
    app.stash_current_edit();

    let label = visible_label(app.visible_tree_items(), path)
        .expect("edited matching part should stay visible in the filtered tree");
    assert!(label.contains(super::tree::EDIT_MARKER), "{label}");
    Ok(())
}

#[test]
fn content_search_runs_in_background_and_filters_matching_parts() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.start_content_search();
    for character in "OOXML TUI".chars() {
        app.content_search_input_char(character);
    }
    pump_until(&mut app, |app| !app.content_search.pending);

    assert!(app.content_search.active);
    assert!(
        app.content_search
            .matches
            .iter()
            .any(|path| path == "/ppt/slides/slide1.xml")
    );
    assert!(app.tree_filter_active());
    assert!(
        app.visible_tree_items()
            .iter()
            .any(|item| item.identifier() == "/ppt")
    );

    app.finish_content_search();
    app.next_content_search_match(false);
    app.cancel_content_search();
    assert!(!app.tree_filter_active());
    assert!(app.content_search.query.is_empty());
    Ok(())
}

#[test]
fn extract_writes_a_byte_identical_part() -> io::Result<()> {
    use std::io::Read as _;

    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.start_extract();
    assert_eq!(app.export.query, "[Content_Types].xml");

    let destination = std::env::temp_dir().join(format!(
        "oox-test-extract-{}-{}.xml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    app.export.query = destination.to_string_lossy().into_owned();
    app.confirm_extract()?;
    pump_until(&mut app, |app| !app.export.pending);

    let Some(PendingExport::Extracted(path)) = app.take_pending_export() else {
        panic!("expected an extracted file");
    };
    assert_eq!(path, destination);

    let mut expected = Vec::new();
    zip::ZipArchive::new(std::fs::File::open("data/sample.pptx")?)?
        .by_name("[Content_Types].xml")?
        .read_to_end(&mut expected)?;
    assert_eq!(std::fs::read(&path)?, expected);

    // Extracting again refuses to clobber the file that was just written.
    app.select_path("/[Content_Types].xml");
    app.export.query = path.to_string_lossy().into_owned();
    app.confirm_extract()?;
    pump_until(&mut app, |app| !app.export.pending);
    assert!(app.take_pending_export().is_none());
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("Export failed"))
    );

    std::fs::remove_file(&path)?;
    Ok(())
}

#[test]
fn copy_produces_pretty_printed_text_for_xml_parts() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.copy_selected_content()?;
    pump_until(&mut app, |app| !app.export.pending);

    match app.take_pending_export() {
        Some(PendingExport::Clipboard(text)) => {
            assert!(text.contains("<Types"));
            assert!(text.contains("\n  "), "expected indented XML, got: {text}");
        }
        _ => panic!("expected clipboard text"),
    }
    Ok(())
}

#[test]
fn status_message_is_visible_without_a_selection() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree.state.select(Vec::new());
    app.status_message = Some("Export failed: boom".to_string());
    assert!(app.selection_status().contains("Export failed: boom"));
    Ok(())
}

#[test]
fn tree_builder_merges_directory_entries_with_their_children() -> io::Result<()> {
    let paths: Vec<String> = [
        "[Content_Types].xml",
        "ppt",
        "ppt/slides",
        "ppt/slides/a.xml",
    ]
    .iter()
    .map(|path| path.to_string())
    .collect();
    let items = create_tree(&paths, &Default::default())?;
    assert_eq!(items.len(), 2);
    let ppt = &items[1];
    assert_eq!(ppt.identifier(), "/ppt");
    assert_eq!(ppt.children().len(), 1);
    assert_eq!(ppt.children()[0].identifier(), "/ppt/slides");
    Ok(())
}

#[test]
fn tree_paths_sort_by_components_before_grouping() -> io::Result<()> {
    let mut paths = ["a/", "a.xml", "a/b.xml"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    super::tree::sort_tree_paths(&mut paths);
    assert_eq!(paths, ["a/", "a/b.xml", "a.xml"]);

    let items = create_tree(&paths, &Default::default())?;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].identifier(), "/a");
    assert_eq!(items[0].children().len(), 2);
    assert_eq!(items[0].children()[1].identifier(), "/a/b.xml");
    assert_eq!(items[1].identifier(), "/a.xml");
    Ok(())
}

fn write_zip(name: &str, entries: &[(&str, &str)]) -> io::Result<std::path::PathBuf> {
    use std::io::Write as _;

    let path = std::env::temp_dir().join(format!("oox-test-{}-{name}", std::process::id()));
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&path)?);
    for (entry, content) in entries {
        writer
            .start_file(*entry, zip::write::SimpleFileOptions::default())
            .map_err(io::Error::other)?;
        writer.write_all(content.as_bytes())?;
    }
    writer.finish().map_err(io::Error::other)?;
    Ok(path)
}

fn editor_text(app: &App) -> String {
    app.editor
        .state
        .lines
        .to_vecs()
        .into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Acceptance for issue #11: comparing a package with a re-saved copy marks
/// added, removed, and changed parts, lists added parts in the tree, and
/// ignores XML that only changed attribute order or whitespace.
#[test]
fn compare_mode_marks_added_removed_and_changed_parts() -> io::Result<()> {
    const CONTENT_TYPES: &str = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/></Types>"#;
    // The same content with reordered attributes and different spacing: the
    // bytes differ, the canonical form does not.
    const CONTENT_TYPES_REFORMATTED: &str = "<?xml version=\"1.0\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n  <Default ContentType=\"application/vnd.openxmlformats-package.relationships+xml\" Extension=\"rels\"/>\n  <Default ContentType=\"application/xml\" Extension=\"xml\"/>\n  <Override ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\" PartName=\"/ppt/presentation.xml\"/>\n</Types>";
    const RELS: &str = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#;

    let before = write_zip(
        "compare-before.pptx",
        &[
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", RELS),
            ("ppt/presentation.xml", "<p:presentation/>"),
            ("ppt/slides/slide1.xml", "<p:sld/>"),
            ("ppt/notes.txt", "notes"),
            ("ppt/aaa.txt", "aaa"),
        ],
    )?;
    let after = write_zip(
        "compare-after.pptx",
        &[
            ("[Content_Types].xml", CONTENT_TYPES_REFORMATTED),
            ("_rels/.rels", RELS),
            (
                "ppt/presentation.xml",
                "<p:presentation><p:sldIdLst/></p:presentation>",
            ),
            ("ppt/slides/slide1.xml", "<p:sld/>"),
            ("ppt/slides/slide2.xml", "<p:sld><p:cSld/></p:sld>"),
            ("ppt/slides/_rels/slide2.xml.rels", RELS),
            ("ppt/notes.txt", "notes"),
        ],
    )?;

    let worker = Worker::start()?;
    let mut app = App::new_loading(
        before.to_string_lossy().into_owned(),
        Some(after.clone()),
        Picker::halfblocks(),
        worker,
    )?;
    pump_until(&mut app, |app| !app.loading);
    assert!(app.is_package_loaded());
    assert!(app.hide_unchanged_parts);
    assert!(app.tree.state.opened().contains(&vec!["/ppt".to_string()]));

    let status = |app: &App, path: &str| {
        app.compare
            .as_ref()
            .and_then(|compare| compare.comparison.status_of(path))
    };
    assert_eq!(
        status(&app, "/ppt/slides/slide2.xml"),
        Some(PartStatus::Added)
    );
    assert_eq!(
        status(&app, "/ppt/slides/_rels/slide2.xml.rels"),
        Some(PartStatus::Added)
    );
    assert_eq!(
        status(&app, "/ppt/presentation.xml"),
        Some(PartStatus::Changed)
    );
    assert_eq!(status(&app, "/ppt/aaa.txt"), Some(PartStatus::Removed));
    assert_eq!(status(&app, "/ppt/notes.txt"), Some(PartStatus::Unchanged));
    assert_eq!(
        status(&app, "/[Content_Types].xml"),
        Some(PartStatus::Unchanged)
    );

    // The tree holds both packages' parts and carries the markers.
    let markers = app.tree_markers();
    assert_eq!(
        markers.get("/ppt/slides/slide2.xml").map(String::as_str),
        Some("+")
    );
    assert!(
        markers
            .get("/ppt/aaa.txt")
            .is_some_and(|marker| marker.contains('-'))
    );
    assert!(
        markers
            .get("/ppt")
            .is_some_and(|marker| marker.contains('*'))
    );
    let added_label = styled_tree_label("slide2.xml", "/ppt/slides/slide2.xml", &markers);
    assert_eq!(
        added_label.spans[0].style.fg,
        Some(ratatui::style::Color::LightGreen)
    );
    let changed_label = styled_tree_label("presentation.xml", "/ppt/presentation.xml", &markers);
    assert_eq!(
        changed_label.spans[0].style.fg,
        Some(ratatui::style::Color::Yellow)
    );
    let removed_label = styled_tree_label("aaa.txt", "/ppt/aaa.txt", &markers);
    assert_eq!(
        removed_label.spans[0].style.fg,
        Some(ratatui::style::Color::LightRed)
    );
    let identifiers = flatten_identifiers(app.visible_tree_items());
    assert!(identifiers.contains(&"/ppt/slides/slide2.xml".to_string()));
    assert!(identifiers.contains(&"/ppt/aaa.txt".to_string()));

    // Compare mode starts with changes only; `u` restores all parts and
    // toggles back to the compact view.
    assert!(app.hide_unchanged_parts);
    assert!(!identifiers.contains(&"/ppt/notes.txt".to_string()));
    app.toggle_unchanged_parts()?;
    let identifiers = flatten_identifiers(app.visible_tree_items());
    assert!(identifiers.contains(&"/ppt/notes.txt".to_string()));
    app.toggle_unchanged_parts()?;
    let identifiers = flatten_identifiers(app.visible_tree_items());
    assert!(!identifiers.contains(&"/ppt/notes.txt".to_string()));

    // Directories that exist only in the comparison package are still
    // recognized as directories when selected.
    app.select_path("/ppt/slides/_rels");
    app.load_selected_file_content()?;
    assert_eq!(
        app.content_message.as_deref(),
        Some("Directory: ppt/slides/_rels")
    );

    // The metadata panel clearly shows both sides and marks a missing part.
    app.select_path("/ppt/slides/slide2.xml");
    let details = app.details_view().text.clone();
    assert!(details.contains("Diff: Added"));
    assert!(details.contains("A — oox-test-") && details.contains("-compare-before.pptx"));
    assert!(details.contains("B — oox-test-") && details.contains("-compare-after.pptx"));
    assert!(details.contains("Part: Absent"));
    assert!(details.contains("Kind: XML"));
    assert!(details.contains("Related parts  [= shared, A/B side-specific]"));
    assert!(details.contains("  [B] OUT rId1"));

    // An unchanged relationship appears once, labeled shared by A and B.
    app.select_path("/ppt/presentation.xml");
    let details = app.details_view().text.clone();
    assert_eq!(details.matches("  [=] IN rId1").count(), 1);

    // The added part previews as a unified diff.
    app.select_path("/ppt/slides/slide2.xml");
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert_eq!(app.preview.kind, PreviewKind::Diff);
    let diff = editor_text(&app);
    assert!(diff.contains("+<p:sld>"), "unexpected diff: {diff}");

    // An unchanged part says so instead of showing a diff.
    app.select_path("/ppt/notes.txt");
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert!(editor_text(&app).contains("No differences"));

    // Filename search and navigation include parts that exist only in B.
    app.start_search();
    for character in "slide2.xml".chars() {
        app.search_input_char(character);
    }
    app.finish_search();
    assert!(
        app.search
            .matches
            .contains(&"/ppt/slides/slide2.xml".to_string())
    );
    app.next_search_match(false);
    assert_eq!(
        app.tree.state.selected().last().unwrap(),
        "/ppt/slides/slide2.xml"
    );
    app.cancel_search();

    // Both package diagnostics and comparison-time diagnostics are visible.
    Arc::make_mut(&mut app.package.as_mut().unwrap().index)
        .warnings
        .push("A package warning".to_string());
    Arc::make_mut(&mut app.package.as_mut().unwrap().index)
        .integrity
        .push(crate::package::Diagnostic::warning(
            "test",
            Some("/ppt/presentation.xml".to_string()),
            "A integrity warning",
        ));
    Arc::make_mut(&mut app.compare.as_mut().unwrap().package.index)
        .warnings
        .push("B package warning".to_string());
    Arc::make_mut(&mut app.compare.as_mut().unwrap().package.index)
        .integrity
        .push(crate::package::Diagnostic::warning(
            "test",
            Some("/ppt/presentation.xml".to_string()),
            "B integrity warning",
        ));
    app.compare
        .as_mut()
        .unwrap()
        .comparison
        .diagnostics
        .push(crate::package::Diagnostic::warning(
            "compare",
            Some("/ppt/presentation.xml".to_string()),
            "Comparison normalization warning",
        ));
    app.details.generation = app.details.generation.wrapping_add(1);
    app.select_path("/ppt/presentation.xml");
    let details = app.details_view().text.clone();
    assert!(details.contains("A warnings") && details.contains("A package warning"));
    assert!(details.contains("B warnings") && details.contains("B package warning"));
    assert!(details.contains("A Integrity issues") && details.contains("A integrity warning"));
    assert!(details.contains("B Integrity issues") && details.contains("B integrity warning"));
    assert!(
        details.contains("Comparison diagnostics")
            && details.contains("Comparison normalization warning")
    );

    std::fs::remove_file(&before)?;
    std::fs::remove_file(&after)?;
    Ok(())
}

// -- Editing and saving ----------------------------------------------------

/// Open a part and replace its buffer with `text`, the way editor input
/// would. Returns the text that was loaded.
fn edit_part(app: &mut App, path: &str, text: &str) -> io::Result<String> {
    app.tree.state.select(vec![path.to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(app);
    let loaded = lines_to_text(&app.editor.state.lines);
    app.editor.state.lines = edtui::Lines::from(text);
    app.refresh_editor_dirty();
    Ok(loaded)
}

fn temp_save_path(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("oox-save-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create temporary save directory");
    directory.join(name)
}

#[test]
fn external_editor_changes_are_read_by_content() {
    use crate::package::MAX_ENTRY_BYTES;
    assert_eq!(pick_up_external("a\n", b"a\n").unwrap(), None);
    // An editor that appends the missing final newline is not a change.
    assert_eq!(pick_up_external("a", b"a\n").unwrap(), None);
    assert_eq!(pick_up_external("a", b"a\r\n").unwrap(), None);
    // A real change is taken over, with Windows line endings normalized.
    assert_eq!(
        pick_up_external("a", b"a\r\nb\r\n").unwrap(),
        Some("a\nb\n".to_string())
    );
    assert_eq!(pick_up_external("a", b"b").unwrap(), Some("b".to_string()));
    // A blank line added to a buffer that already ended in a newline is an
    // edit, not the tolerated missing-newline fixup.
    assert_eq!(
        pick_up_external("a\n", b"a\n\n").unwrap(),
        Some("a\n\n".to_string())
    );
    assert_eq!(
        pick_up_external("a\n", b"a\nb\n").unwrap(),
        Some("a\nb\n".to_string())
    );
    // Windows endings in the buffer are normalized before comparing, so an
    // untouched round trip through a CRLF-normalizing editor is still a no-op.
    assert_eq!(pick_up_external("a\r\nb", b"a\nb\n").unwrap(), None);
    assert!(pick_up_external("a", b"\xff").is_err());
    // The bound matches the preview read limit, so any part that could be
    // opened can also be taken back.
    assert!(pick_up_external("a", &vec![b'x'; MAX_ENTRY_BYTES as usize + 1]).is_err());
}

#[test]
fn external_edit_replaces_the_buffer_and_marks_the_part() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let loaded = edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>typed</a>")?;
    assert!(app.editor_is_dirty());

    // A change is taken into the same unsaved buffer, so it still needs a save.
    let changed = "<a>from the editor</a>";
    app.apply_external_edit("<a>typed</a>", changed.as_bytes(), true);
    assert!(app.editor_is_dirty());
    assert_eq!(lines_to_text(&app.editor.state.lines), changed);

    // An unchanged file is a no-op.
    app.apply_external_edit(changed, changed.as_bytes(), true);
    assert!(app.editor_is_dirty());
    assert_eq!(lines_to_text(&app.editor.state.lines), changed);

    // An aborted editor (`:cq`) discards whatever it left behind.
    app.apply_external_edit(changed, b"<a>aborted</a>", false);
    assert_eq!(lines_to_text(&app.editor.state.lines), changed);

    // The baseline is untouched, so undo back to the original clears the part.
    app.apply_external_edit(changed, loaded.as_bytes(), true);
    assert!(!app.editor_is_dirty());
    assert!(!app.has_unsaved_edits());
    Ok(())
}

#[test]
fn unsaved_edits_survive_navigation() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    assert_eq!(app.unsaved_edit_count(), 1);
    // The marker the tree labels are built from.
    assert!(app.tree_markers().contains_key("/ppt/slides/slide1.xml"));

    // Another part shows its own text, and the edit is still counted.
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert!(!app.editor_is_dirty());
    assert_eq!(app.unsaved_edit_count(), 1);
    assert!(!lines_to_text(&app.editor.state.lines).contains("edited"));
    assert!(app.tree_markers().contains_key("/ppt/slides/slide1.xml"));

    // Coming back restores the buffer instead of reloading from disk.
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    app.load_selected_file_content()?;
    assert!(app.editor_is_dirty());
    assert_eq!(lines_to_text(&app.editor.state.lines), "<a>edited</a>");

    // The summary view is a detour, not a discard.
    app.toggle_summary()?;
    assert_eq!(app.unsaved_edit_count(), 1);
    app.toggle_summary()?;
    assert_eq!(lines_to_text(&app.editor.state.lines), "<a>edited</a>");
    Ok(())
}

#[test]
fn only_a_matching_confirmation_action_keeps_the_guard_armed() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    assert!(!app.request_quit());
    // A globally-bound Quit that is invalid in this editor context is not
    // confirmation; it must disarm rather than authorize a later quit.
    app.disarm_confirmation_unless(false, false);
    assert!(!app.request_quit());
    app.disarm_confirmation_unless(true, false);
    assert!(app.request_quit(), "a valid second quit confirms");

    app.disarm_confirmation();
    app.request_revert();
    // A valid quit action cannot confirm a pending revert.
    app.disarm_confirmation_unless(true, false);
    app.request_revert();
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("again")),
        "the revert must ask again after an unrelated confirmation action"
    );
    Ok(())
}

#[test]
fn quitting_with_unsaved_edits_is_asked_twice() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    // Nothing to lose: the first quit goes through.
    assert!(app.request_quit());
    app.disarm_confirmation();

    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    assert!(!app.request_quit());
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("unsaved edit"))
    );
    // Any other key cancels the confirmation, so it cannot be tripped later.
    app.disarm_confirmation();
    assert!(!app.request_quit());
    // The confirmation is a deliberate second press.
    assert!(app.request_quit());
    Ok(())
}

#[test]
fn no_edits_means_nothing_to_save() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    app.start_save();
    assert!(!app.save.active);
    assert_eq!(app.status_message.as_deref(), Some("No edits to save"));
    Ok(())
}

/// The write completing must not unlock input before the newly written
/// package and reselected part preview have been installed.
#[test]
fn save_lock_spans_reindex_and_reselected_preview() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    let target = temp_save_path("reload-lock.pptx");
    let _ = std::fs::remove_file(&target);
    std::fs::copy("data/sample.pptx", &target)?;

    // Exercise the post-write phase directly: the Open job has not yet
    // returned, so its old package/buffers are still live.
    app.after_save(target.clone());
    assert!(
        app.is_saving(),
        "reload must remain covered by the save lock"
    );
    app.start_save();
    assert!(
        !app.save.active,
        "a second save cannot start during re-index"
    );
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"));
    app.request_revert();
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"));
    app.request_external_edit()?;
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"));
    assert!(app.editor.external_edit_request.is_none());

    pump_until(&mut app, |app| {
        !app.is_saving() && !app.loading && !app.preview.pending
    });
    assert_eq!(app.file_path, target.to_string_lossy());
    assert_eq!(
        app.editor.previewed_path.as_deref(),
        Some("/ppt/slides/slide1.xml")
    );
    assert!(!app.has_unsaved_edits());
    std::fs::remove_file(&target)?;
    Ok(())
}

#[test]
fn save_reload_failure_releases_the_input_lock() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let missing = temp_save_path("missing-reload.pptx");
    let _ = std::fs::remove_file(&missing);

    app.after_save(missing);
    assert!(app.is_saving());
    pump_until(&mut app, |app| !app.is_saving() && !app.loading);
    assert!(app.worker_error.is_some());
    Ok(())
}

/// The acceptance path from the issue: edit a slide, save a new package, and
/// check that it opens with the change and that nothing else moved.
#[test]
fn saving_writes_the_edit_and_leaves_other_parts_byte_identical() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let loaded = edit_part(
        &mut app,
        "/ppt/slides/slide1.xml",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<p:sld><a:t>EDITED TUI</a:t></p:sld>",
    )?;
    assert!(loaded.contains("OOXML") || loaded.contains("p:sld"));

    let target = temp_save_path("out.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    pump_until(&mut app, |app| !app.save.pending);
    // The saved package is re-indexed through the worker and the part is
    // shown again, so the tree, diagnostics and summary all describe it.
    pump_until(&mut app, |app| {
        !app.loading && !app.preview.pending && app.editor.previewed_path.is_some()
    });
    assert!(!app.has_unsaved_edits());
    assert_eq!(app.file_path, target.to_string_lossy());
    assert_eq!(
        app.editor.previewed_path.as_deref(),
        Some("/ppt/slides/slide1.xml")
    );
    // Saving again writes the same file rather than stacking `.edited`.
    assert_eq!(app.default_save_target(), target.to_string_lossy());

    use std::io::Read as _;
    let mut original = zip::ZipArchive::new(std::fs::File::open("data/sample.pptx")?)?;
    let mut saved = zip::ZipArchive::new(std::fs::File::open(&target)?)?;
    assert_eq!(original.len(), saved.len());
    for index in 0..original.len() {
        let mut before = original.by_index(index)?;
        let mut after = saved.by_index(index)?;
        let before_name = before.name().map_err(io::Error::other)?.into_owned();
        let after_name = after.name().map_err(io::Error::other)?.into_owned();
        assert_eq!(before_name, after_name, "entry order is preserved");
        let mut before_bytes = Vec::new();
        before.read_to_end(&mut before_bytes)?;
        let mut after_bytes = Vec::new();
        after.read_to_end(&mut after_bytes)?;
        if before_name == "ppt/slides/slide1.xml" {
            assert_ne!(before_bytes, after_bytes);
            assert_eq!(before.compression(), after.compression());
            assert!(String::from_utf8_lossy(&after_bytes).contains("EDITED TUI"));
        } else {
            assert_eq!(before.compression(), after.compression());
            assert_eq!(
                before_bytes, after_bytes,
                "{} must be untouched",
                before_name
            );
        }
    }

    std::fs::remove_file(&target)?;
    Ok(())
}

/// A request that could not be served must not fire later, when the same
/// part happens to be opened again.
#[test]
fn an_abandoned_external_edit_request_is_forgotten() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    // Ask for a part, then move to another one before it can be handed over.
    app.tree
        .state
        .select(vec!["/ppt/presentation.xml".to_string()]);
    app.request_external_edit()?;
    preview_loaded(&mut app);
    assert!(app.external_edit_ready());
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    assert!(!app.external_edit_ready());
    assert!(app.take_external_edit_request().is_none());
    Ok(())
}

/// A save cannot snapshot buffers while an external edit request is still
/// waiting for its asynchronous preview to arrive.
#[test]
fn deferred_external_edit_blocks_save_prompt() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    app.tree
        .state
        .select(vec!["/ppt/presentation.xml".to_string()]);
    app.request_external_edit()?;
    assert!(app.editor.external_edit_request.is_some());
    assert!(!app.external_edit_ready(), "the preview is still pending");

    app.start_save();
    assert!(!app.save.active, "saving must wait for the deferred edit");
    assert_eq!(
        app.status_message.as_deref(),
        Some("External edit is still loading")
    );
    Ok(())
}

#[test]
fn external_edit_request_is_refused_while_save_prompt_is_open() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    app.save.active = true;
    app.request_external_edit()?;
    assert!(app.editor.external_edit_request.is_none());
    assert_eq!(app.status_message.as_deref(), Some("Save prompt is active"));
    Ok(())
}

#[test]
fn external_edit_snapshot_keeps_the_part_name() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>snapshot</a>")?;
    let pending = app.begin_external_edit()?;
    // The editor picks its syntax mode from the extension.
    let name = pending
        .temp
        .path()
        .file_name()
        .expect("snapshot has a file name")
        .to_string_lossy()
        .to_string();
    assert!(name.ends_with("slide1.xml"), "{name}");
    assert_eq!(
        std::fs::read_to_string(pending.temp.path())?,
        pending.written
    );
    assert_eq!(pending.written, "<a>snapshot</a>");
    Ok(())
}

#[test]
fn an_existing_target_is_never_overwritten_without_a_second_confirm() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    // The source file itself is the dangerous target.
    let victim = temp_save_path("existing.pptx");
    std::fs::copy("data/sample.pptx", &victim)?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    app.save.query = victim.to_string_lossy().to_string();
    app.save.active = true;
    app.confirm_save()?;
    // Still prompting, and the file on disk is untouched.
    assert!(app.save.active);
    assert!(!app.save.pending);
    assert_eq!(
        std::fs::read("data/sample.pptx")?,
        std::fs::read(&victim)?,
        "the target must not be written before the confirmation"
    );
    // The prompt itself carries the question; the status bar is not used for
    // it, because the save prompt replaces the whole status line.
    assert!(
        app.selection_status().contains("exists"),
        "prompt did not ask: {}",
        app.selection_status()
    );

    // The second confirm goes ahead.
    app.confirm_save()?;
    assert!(!app.save.active);
    pump_until(&mut app, |app| !app.save.pending);
    pump_until(&mut app, |app| !app.loading);
    assert_ne!(std::fs::read("data/sample.pptx")?, std::fs::read(&victim)?);

    std::fs::remove_file(&victim)?;
    Ok(())
}

/// A path written by this session is only a default, not lasting overwrite
/// authorization: another process may replace the file before the next save.
#[test]
fn a_previously_saved_target_requires_confirmation_again() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let target = temp_save_path("previously-saved.pptx");
    let _ = std::fs::remove_file(&target);
    std::fs::copy("data/sample.pptx", &target)?;
    let external_contents = std::fs::read(&target)?;
    app.save.last_saved = Some(target.clone());
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>second edit</a>")?;

    app.start_save();
    assert!(app.save.active);
    assert_eq!(app.save.query, target.to_string_lossy());
    app.confirm_save()?;

    assert!(app.save.active, "the overwrite confirmation must stay open");
    assert!(!app.save.pending);
    assert_eq!(std::fs::read(&target)?, external_contents);
    assert!(
        app.selection_status().contains("exists"),
        "prompt did not ask: {}",
        app.selection_status()
    );

    app.confirm_save()?;
    pump_until(&mut app, |app| !app.save.pending);
    pump_until(&mut app, |app| !app.loading);
    assert_ne!(std::fs::read(&target)?, external_contents);
    std::fs::remove_file(&target)?;
    Ok(())
}

#[test]
fn malformed_xml_still_saves_with_a_warning() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<p:sld><a:t>unclosed")?;

    // The verdict is advisory and is shown before the write, while the user
    // can still back out.
    app.start_save();
    assert!(
        app.selection_status().contains("not well-formed"),
        "prompt did not warn: {}",
        app.selection_status()
    );

    let target = temp_save_path("broken.pptx");
    let _ = std::fs::remove_file(&target);
    app.save.query = target.to_string_lossy().to_string();
    app.confirm_save()?;
    pump_until(&mut app, |app| !app.save.pending);
    pump_until(&mut app, |app| !app.loading);
    assert!(target.exists(), "the user's text must not be trapped");

    std::fs::remove_file(&target)?;
    Ok(())
}

/// A save owns the buffers until it lands: editor input typed meanwhile would
/// be thrown away by the reload, so every mutating entry point is refused.
#[test]
fn a_save_in_flight_blocks_further_edits() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    let target = temp_save_path("locked.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    // Without pumping, so the job is still in flight.
    assert!(app.is_saving());

    app.start_save();
    assert!(!app.save.active, "a second prompt must not open");
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"));

    app.request_revert();
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"),);
    assert_eq!(app.unsaved_edit_count(), 1, "the edit must survive");

    app.request_external_edit()?;
    assert_eq!(app.status_message.as_deref(), Some("Save in progress"),);

    pump_until(&mut app, |app| !app.save.pending);
    pump_until(&mut app, |app| !app.loading);
    assert!(target.exists());
    std::fs::remove_file(&target)?;
    Ok(())
}

/// `R` discards the unsaved edits of the selected part, but only after a
/// second press, and it works for a stashed edit as well as the one on screen.
#[test]
fn revert_discards_the_edits_of_the_part_on_screen() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let loaded = edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    app.request_revert();
    assert!(app.editor_is_dirty(), "one press must not discard anything");
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.contains("again")),
        "the first press must ask for a second one"
    );

    app.request_revert();
    assert!(!app.editor_is_dirty());
    assert_eq!(lines_to_text(&app.editor.state.lines), loaded);
    assert!(!app.has_unsaved_edits());
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reverted ppt/slides/slide1.xml")
    );
    Ok(())
}

/// Ordinary editor events do not re-scan a sticky dirty buffer; an undo/redo
/// event does, so returning exactly to baseline clears the marker.
#[test]
fn dirty_state_is_reconciled_only_on_content_or_history_events() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    let baseline = app.editor.baseline.clone().expect("loaded baseline");
    assert!(app.editor_is_dirty());

    app.editor.state.lines = baseline;
    app.refresh_editor_dirty_after_input(false, false);
    assert!(
        app.editor_is_dirty(),
        "ordinary events keep the sticky flag"
    );
    app.refresh_editor_dirty_after_input(false, true);
    assert!(
        !app.editor_is_dirty(),
        "undo reconciliation clears baseline"
    );
    Ok(())
}

#[test]
fn revert_discards_a_stashed_edit() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    // Move away, which sets the edit aside, then come back to select it.
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    app.load_selected_file_content()?;
    preview_loaded(&mut app);
    app.tree
        .state
        .select(vec!["/ppt/slides/slide1.xml".to_string()]);
    assert_eq!(app.unsaved_edit_count(), 1);

    app.request_revert();
    app.request_revert();
    assert!(!app.has_unsaved_edits(), "the stashed edit must be gone");
    assert_eq!(
        app.status_message.as_deref(),
        Some("Reverted ppt/slides/slide1.xml")
    );

    // Reverting a part with nothing to discard says so instead of arming.
    app.request_revert();
    assert_eq!(
        app.status_message.as_deref(),
        Some("No unsaved edits in ppt/slides/slide1.xml")
    );
    Ok(())
}

/// The reload after a save must not throw the cursor back to the top of the
/// file the user was editing.
#[test]
fn cursor_survives_a_save() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let loaded = edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    // Enough lines that row 3 exists after the reload too.
    app.editor.state.lines = edtui::Lines::from(format!("{loaded}<!-- x -->"));
    app.refresh_editor_dirty();
    app.editor.state.cursor = edtui::Index2::new(3, 2);

    let target = temp_save_path("cursor.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    pump_until(&mut app, |app| {
        !app.save.pending && !app.loading && !app.preview.pending
    });
    assert_eq!(
        app.editor.previewed_path.as_deref(),
        Some("/ppt/slides/slide1.xml")
    );
    assert_eq!(app.editor.state.cursor, edtui::Index2::new(3, 2));

    std::fs::remove_file(&target)?;
    Ok(())
}

/// Insert mode may sit one past the last character; a save from there must
/// leave the cursor at the end of the line and the editor still in insert
/// mode, rather than pulling it back one column into normal mode.
#[test]
fn an_insert_mode_cursor_at_end_of_line_survives_a_save() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    let loaded = edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;
    app.editor.state.lines = edtui::Lines::from(format!("{loaded}<!-- x -->"));
    app.refresh_editor_dirty();
    let end_of_row = app.editor.state.lines.len_col(3).expect("row 3 exists");
    app.editor.state.mode = edtui::EditorMode::Insert;
    app.editor.state.cursor = edtui::Index2::new(3, end_of_row);

    let target = temp_save_path("insert-cursor.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    pump_until(&mut app, |app| {
        !app.save.pending && !app.loading && !app.preview.pending
    });
    assert_eq!(app.editor.state.mode, edtui::EditorMode::Insert);
    assert_eq!(app.editor.state.cursor, edtui::Index2::new(3, end_of_row));

    std::fs::remove_file(&target)?;
    Ok(())
}

/// The external-editor path replaces the buffer in place, so it must keep
/// the same insert-mode end-of-line position as a package reload does.
#[test]
fn an_insert_mode_cursor_survives_an_external_edit() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>typed</a>")?;
    let written = lines_to_text(&app.editor.state.lines);
    let end_of_line = app.editor.state.lines.len_col(0).expect("one line");
    app.editor.state.mode = edtui::EditorMode::Insert;
    app.editor.state.cursor = edtui::Index2::new(0, end_of_line);

    app.apply_external_edit(&written, b"<a>external edit</a>", true);

    assert_eq!(app.editor.state.mode, edtui::EditorMode::Insert);
    assert_eq!(app.editor.state.cursor, edtui::Index2::new(0, end_of_line));
    assert_eq!(
        lines_to_text(&app.editor.state.lines),
        "<a>external edit</a>"
    );
    Ok(())
}

/// The bounds a restored cursor is clamped to depend on the mode it is
/// restored into.
#[test]
fn restored_cursor_uses_the_bounds_of_its_mode() {
    let spot = |row, col, mode| CursorSpot {
        cursor: edtui::Index2::new(row, col),
        viewport: (0, 0),
        mode,
    };
    let fresh = || edtui::EditorState::new(edtui::Lines::from("abc\nde"));

    // Insert mode keeps the position after the last character.
    let mut state = fresh();
    place_cursor(&mut state, spot(0, 3, edtui::EditorMode::Insert));
    assert_eq!(state.mode, edtui::EditorMode::Insert);
    assert_eq!(state.cursor, edtui::Index2::new(0, 3));

    // Normal mode must sit on a character.
    let mut state = fresh();
    place_cursor(&mut state, spot(0, 3, edtui::EditorMode::Normal));
    assert_eq!(state.mode, edtui::EditorMode::Normal);
    assert_eq!(state.cursor, edtui::Index2::new(0, 2));

    // A position beyond a shorter buffer is clamped per mode.
    let mut state = fresh();
    place_cursor(&mut state, spot(9, 9, edtui::EditorMode::Normal));
    assert_eq!(state.cursor, edtui::Index2::new(1, 1));
    let mut state = fresh();
    place_cursor(&mut state, spot(1, 9, edtui::EditorMode::Insert));
    assert_eq!(state.cursor, edtui::Index2::new(1, 2));

    // Insert mode also permits the virtual row after the final line.
    let mut state = fresh();
    place_cursor(&mut state, spot(2, 0, edtui::EditorMode::Insert));
    assert_eq!(state.mode, edtui::EditorMode::Insert);
    assert_eq!(state.cursor, edtui::Index2::new(2, 0));

    // Visual mode has no selection to come back to, so it lands in normal.
    let mut state = fresh();
    place_cursor(&mut state, spot(0, 1, edtui::EditorMode::Visual));
    assert_eq!(state.mode, edtui::EditorMode::Normal);
    assert_eq!(state.cursor, edtui::Index2::new(0, 1));
}

/// Copying a part with unsaved edits copies what is on screen, not the bytes
/// that are still on disk.
#[test]
fn copy_uses_the_unsaved_buffer() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>buffer</a>")?;
    app.copy_selected_content()?;
    match app.take_pending_export() {
        Some(PendingExport::Clipboard(text)) => assert_eq!(text, "<a>buffer</a>"),
        _ => panic!("expected the buffer text on the clipboard"),
    }
    Ok(())
}

/// A save in flight owns `previewed_path`: navigation would move the buffer
/// into `edits` and make the reload reselect the wrong part. The event loop
/// drops input while `is_saving()`, so this pins the state the guard protects.
#[test]
fn navigation_would_break_the_reselect_so_input_is_frozen() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    let target = temp_save_path("frozen.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    // The job is still in flight, and this is exactly what the loop checks
    // before dispatching any event.
    assert!(app.is_saving());

    // The hazard the guard removes: moving on now would stash the buffer and
    // clear the path the reload needs to reselect.
    app.tree
        .state
        .select(vec!["/[Content_Types].xml".to_string()]);
    let _ = app.load_selected_file_content();
    assert!(
        app.editor.previewed_path.is_none(),
        "this is why input must be dropped while saving"
    );

    pump_until(&mut app, |app| !app.save.pending && !app.loading);
    assert!(target.exists());
    std::fs::remove_file(&target)?;
    Ok(())
}

/// A target that appeared after the user was asked must not be clobbered: the
/// authorization is decided at confirmation time and carried into the job.
#[test]
fn a_target_created_after_confirmation_is_not_replaced() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    // The user confirms a target that does not exist yet, without overwrite
    // rights, exactly as `confirm_save` computes them for a fresh path.
    let target = temp_save_path("appeared.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), false)?;
    // Something else claims the path before the worker installs.
    std::fs::write(&target, b"not ours")?;

    pump_until(&mut app, |app| !app.save.pending);
    assert_eq!(
        std::fs::read(&target)?,
        b"not ours",
        "the file that appeared must survive"
    );
    assert!(
        app.status_message
            .as_deref()
            .is_some_and(|message| message.starts_with("Save failed:")),
        "the refusal must be reported: {:?}",
        app.status_message
    );

    std::fs::remove_file(&target)?;
    Ok(())
}

/// A new target must not be written owner-only just because the temporary
/// file it is built from is.
#[cfg(unix)]
#[test]
fn a_new_target_inherits_the_source_permissions() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    let target = temp_save_path("perm.pptx");
    let _ = std::fs::remove_file(&target);
    app.submit_save(target.clone(), true)?;
    pump_until(&mut app, |app| !app.save.pending);

    let written = std::fs::metadata(&target)?.permissions().mode() & 0o777;
    let source = std::fs::metadata("data/sample.pptx")?.permissions().mode() & 0o777;
    assert_eq!(
        written, source,
        "the new file must match the package it came from"
    );

    std::fs::remove_file(&target)?;
    Ok(())
}

/// Replacing a symlink must not inherit the mode of its referent: the
/// installed target is a regular file, so use the package's mode instead.
#[cfg(unix)]
#[test]
fn overwriting_a_symlink_does_not_copy_referent_permissions() -> io::Result<()> {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    let target = temp_save_path("symlink-target.pptx");
    let referent = temp_save_path("symlink-referent.pptx");
    let _ = std::fs::remove_file(&target);
    let _ = std::fs::remove_file(&referent);
    std::fs::write(&referent, b"referent")?;
    let source_mode = std::fs::metadata("data/sample.pptx")?.permissions().mode() & 0o777;
    let referent_mode = if source_mode == 0o600 { 0o644 } else { 0o600 };
    std::fs::set_permissions(&referent, std::fs::Permissions::from_mode(referent_mode))?;
    symlink(&referent, &target)?;

    app.submit_save(target.clone(), true)?;
    pump_until(&mut app, |app| {
        !app.is_saving() && !app.loading && !app.preview.pending
    });

    let installed = std::fs::symlink_metadata(&target)?;
    assert!(
        installed.file_type().is_file(),
        "symlink should be replaced"
    );
    let installed_mode = installed.permissions().mode() & 0o777;
    assert_eq!(installed_mode, source_mode);
    assert_ne!(
        installed_mode, referent_mode,
        "must not inherit referent mode"
    );

    std::fs::remove_file(&target)?;
    std::fs::remove_file(&referent)?;
    Ok(())
}

/// A dangling symlink occupies the path but `exists()` reports false, so it
/// would otherwise be replaced without the second confirmation.
#[cfg(unix)]
#[test]
fn a_dangling_symlink_still_asks_before_it_is_replaced() -> io::Result<()> {
    let mut app = test_app("data/sample.pptx")?;
    edit_part(&mut app, "/ppt/slides/slide1.xml", "<a>edited</a>")?;

    let target = temp_save_path("dangling.pptx");
    let _ = std::fs::remove_file(&target);
    let missing = temp_save_path("does-not-exist.pptx");
    let _ = std::fs::remove_file(&missing);
    std::os::unix::fs::symlink(&missing, &target)?;
    assert!(!target.exists(), "the symlink must dangle for this test");

    app.save.query = target.to_string_lossy().to_string();
    app.save.active = true;
    app.confirm_save()?;
    assert!(app.save.active, "the prompt must stay open");
    assert!(
        !app.save.pending,
        "nothing may be written before confirming"
    );
    assert!(
        app.selection_status().contains("exists"),
        "prompt did not ask: {}",
        app.selection_status()
    );
    assert!(
        std::fs::symlink_metadata(&target)?.file_type().is_symlink(),
        "the symlink must survive the refused write"
    );

    std::fs::remove_file(&target)?;
    Ok(())
}
