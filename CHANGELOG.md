# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.1] - Unreleased

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

[0.2.1]: https://github.com/sergey-tihon/oox/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sergey-tihon/oox/releases/tag/v0.2.0
