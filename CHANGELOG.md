# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Removed

- Remove the Repo Assist GitHub Agentic Workflow and its generated lock files.

### Added

- Report OSC 7577 resume commands for the open file and optional comparison/config in supporting terminals.

### Fixed

- Fall back to the running executable path when oox is launched by an explicit path or build tool.
- Preserve mixed-content XML when formatting editable parts, and retain filtered tree labels after edits.
- Ignore mouse motion for focus changes, clamp metadata-link navigation, and restore the terminal after panics.
- Bound external-editor reads, tolerate unreadable content-search parts, and report successful no-clobber saves correctly.
- Require a known OPC `officeDocument` relationship type.
- Keep directory paths grouped in package trees, summary links aligned across embedded newlines, and cancel content searches when cleared or dismissed.
- Ignore embedded ZIP records during archive preflight and activate the metadata link highlighted after selection changes.
- Discover Word, Excel, and PowerPoint summaries and Excel shared strings through OPC relationships and content types.
- Bound central-directory checks, diff computation, and summary parsing; narrow live path filtering without lowercasing every part.
- Reject malformed editor modes and conflicting keybindings, and hit-test metadata links using rendered scroll and terminal display cells.
- Coalesce superseded package, preview, and content-search work to keep rapid navigation responsive.

## [0.2.2] - 2026-11-22

### Fixed

- Editor yanks use the shared system clipboard across buffers and instances, with an in-process fallback when unavailable.

## [0.2.1] - 2026-10-06

### Changed

- Licensed under `MIT OR Apache-2.0` (previously MIT only).
- Minimum supported Rust version is now 1.90; 1.86 was declared but never built with the locked dependencies.
- Rewrote the README and added a demo; refreshed the crate description, keywords, and categories.
- `oox --help` lists usage examples.
- Clarified the XDG-aware configuration path and image preview scaling in the README.

### Fixed

- README listed `e` as the extract key; extract is `x`.
- `--generate-config` accepts `--config <PATH>` without requiring a document argument.

## [0.2.0] - 2026-10-06

First release on crates.io.

### Added

- Package tree with XML/JSON pretty-printing and syntax highlighting.
- Image previews (PNG, JPEG, GIF, BMP, WebP), hex and metadata views for binary parts.
- Path search and background content search.
- Relationship navigation from `r:id`/`r:embed` references.
- Word, Excel, and PowerPoint document summaries.
- Package integrity checks.
- Two-package comparison with an XML-aware diff.
- Part export, `$PAGER`/`$EDITOR` integration, and clipboard copy.
- Editing and saving parts to a new package.
- Configurable key bindings with Vim or Emacs editor mode.

[0.2.2]: https://github.com/sergey-tihon/oox/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sergey-tihon/oox/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sergey-tihon/oox/releases/tag/v0.2.0
