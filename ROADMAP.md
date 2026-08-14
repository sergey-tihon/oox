# oox Roadmap

`oox` is a read-only OOXML package inspector. The current product is strong at package-level inspection—parts, relationships, XML, embedded images, raw previews, and lightweight document summaries—but it intentionally does not render Office documents as users see them.

## Current status

### Completed

- ZIP package tree inspection for `.pptx`, `.docx`, and `.xlsx` files.
- XML pretty-printing with syntax highlighting.
- PNG, JPEG, GIF, BMP, and WebP image previews.
- Plain-text, JSON, `.bin` hex, media, font, OLE, and generic binary previews.
- Content-type and relationship parsing.
- Incoming/outgoing relationship navigation.
- PowerPoint, Word, and Excel lightweight summaries.
- Search, next/previous match navigation, expand/collapse all, help overlay, configurable keybindings, Vim/Emacs editor modes, and basic mouse support.
- Background package indexing, summary generation, and part previews.
- Stale worker-result rejection and loading/error states.
- Shared layout geometry for rendering and hit testing.
- RAII terminal restoration.
- Bounded archive and preview processing.
- Worker-side ZIP archive caching; the package index is shared with the worker via `Arc` instead of being cloned per preview request.
- Cached metadata panel rendering and a redraw-on-change event loop (no idle 20 fps redraws).
- Preview classification/formatting split into `src/preview.rs`; PowerPoint/Word/Excel parsers split into `src/summary/` modules; duplicated `Package`/`PackageIndex` state removed from `App`.
- Minimum-terminal-size guard, capped navigation history, generated help overlay, read-only content labeling, and F1 help in Emacs editor mode.
- Live visual tree filtering: non-matching paths are hidden while typing a search query, the selection follows the first match, Enter keeps the filter for n/N cycling, and Esc restores the full tree and its pre-search open/closed state.

## Most important remaining features

### Priority 1 — Document rendering

**Status: not implemented; intentionally postponed.**

Add a separate rendering layer that displays documents closer to their Office appearance:

- PowerPoint slide layouts, text boxes, themes, fonts, images, tables, charts, and speaker notes.
- Word paragraphs, styles, headers/footers, tables, lists, images, hyperlinks, and section layout.
- Excel worksheets with cell formatting, merged cells, formulas, charts, and sheet layout.

This should not be added to the current package-inspection code. It needs independent document models, rendering backends, and a clear fallback for unsupported OOXML features.

### Priority 2 — Better package navigation and inspection

- **Content search:** grep-style search across part contents in the background worker, not only package-path matching.
- **Resizable panes:** allow users to adjust tree, metadata, and content widths/heights.
- **Bookmarks or pinned parts:** keep frequently inspected package parts accessible.
- **Relationship graph view:** show a navigable graph for slides, worksheets, document parts, images, and layouts.
- **Multiple documents/tabs:** inspect more than one package without restarting the application.
- **Directory aggregates:** show child counts, total uncompressed size, and part-kind breakdown for directories in the metadata panel.
- **Reload:** re-open the package on demand (`R`) or when the file changes on disk.
- **Image zoom/pan:** toggle fit-to-pane versus actual size for image previews.

### Priority 3 — Richer document summaries

Keep summaries lightweight, but expand their coverage before full rendering:

#### PowerPoint

- Slide dimensions and layout names.
- Text grouped by shape rather than flattened title extraction.
- Hyperlinks, charts, tables, and embedded objects.
- Theme and master/layout relationships.
- Optional slide thumbnails.

#### Word

- Headers, footers, sections, lists, hyperlinks, comments, footnotes, and tracked-change indicators.
- More accurate paragraph text handling, line breaks, tabs, and nested tables.
- Document properties and custom metadata.

#### Excel

- Hidden/very-hidden sheets and workbook properties.
- Cell types, styles, merged ranges, hyperlinks, comments, named ranges, and tables.
- Formula/result distinction and calculation metadata.
- Charts, drawings, and external links.

### Priority 4 — Embedded content workflows

- Open a selected media or embedded object with the system application, behind an explicit user action.
- Export selected package parts to a file or directory.
- Optional media playback integration rather than only metadata display.
- Safer inspection of VBA/OLE content without executing it.
- Copy the selected part path or content to the system clipboard.
- Toggle hex view for any part (not only `.bin`), and fall back to raw unformatted text when XML pretty-printing fails on malformed input.
- Detect encrypted OOXML and legacy OLE compound files and explain the situation instead of showing a raw ZIP error.
- Preview SVG images and report EMF/WMF/TIFF parts with clearer messaging.

These operations should remain opt-in and must never execute embedded content automatically.

## Engineering backlog

These are not user-visible features, but they will improve future work:

- Preserve `PathBuf` internally instead of converting document paths to lossy display strings.
- Add a larger fixture corpus covering malformed packages, namespace variations, macros, charts, external links, and large archives; cover the Word and Excel summary paths with real `.docx`/`.xlsx` fixtures (only `data/sample.pptx` exists today).
- Add benchmarks for startup indexing, summary generation, large XML previews, and image decoding.
- Add UI/event integration tests for focus transitions, loading states, mouse interaction, and terminal setup failures.
- Consider cooperative cancellation inside long-running parser and image operations.
- Add a true read-only mode for the content editor: `edtui` has no read-only support, so preview text can be "edited" in the UI even though changes are silently discarded. Either contribute read-only support upstream, intercept mutating keys before dispatch, or replace the viewer widget. The content pane title is labeled "(read-only)" until this is fixed.

## Explicitly deferred

### Editing and saving

Editing OOXML directly can invalidate relationships, content types, namespaces, styles, and package metadata. The inspector remains read-only until there is a concrete editing workflow and a package-rewrite validation layer.

### Full Office compatibility

The application should not promise complete support for every Office extension, producer, version, or malformed package. Unsupported parts should continue to produce bounded previews and diagnostics rather than crashes.

## Reliability limits

Current safety limits are deliberately conservative:

- 100,000 archive entries maximum.
- 256 MiB declared uncompressed package total maximum.
- 32 MiB decompressed part-preview limit.
- 4 MiB metadata-part and formatted XML/JSON preview limits.
- 64 path components maximum.
- 8,192 × 8,192 image dimensions and 16 million image pixels maximum.
- Bounded summary output and extracted item/text collections.

If these limits become configurable, configuration must not silently disable safety checks; the UI should clearly report truncated or rejected content.

## Suggested order

1. Improve navigation with visual filtering and resizable panes.
2. Add richer summaries and optional slide thumbnails.
3. Add safe export/open workflows for embedded parts.
4. Build an independent document-rendering layer.
5. Revisit editing only after package rewriting and validation requirements are understood.
