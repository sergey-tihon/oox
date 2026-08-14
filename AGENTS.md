# AGENTS.md

## Project

`ooxml-tui` is a Rust terminal app for inspecting Office Open XML documents (`.pptx`, `.docx`, `.xlsx`). It opens the OOXML ZIP package, lists its parts in a tree, previews XML content with syntax highlighting, and displays common embedded images.

## Architecture

- `src/main.rs` — Entry point, terminal setup/teardown, event loop.
- `src/app.rs` — `App` state: zip tree, tree selection, editor state, file loading, navigation history, search, cached metadata view.
- `src/package.rs` — Canonical package model (`Package`, `PackageIndex`), bounded ZIP access, content-type/relationship parsing, and the single source of truth for extension- and content-type-based part classification.
- `src/preview.rs` — Part-preview classification and formatters (XML/JSON pretty-print, hex dump, binary info), with bounded output writers.
- `src/summary/` — Document summary view model (`mod.rs`) and per-format parsers (`ppt.rs`, `word.rs`, `excel.rs`).
- `src/worker.rs` — Background worker thread; owns a cached `ZipArchive` handle and receives the shared `Arc<PackageIndex>`.
- `src/keybindings.rs` — Configurable `Action` bindings, editor mode, and generated help-overlay content.
- `src/layout.rs` — Shared layout geometry used by both rendering and mouse hit testing.
- `src/ui.rs` — Ratatui layout and widgets (tree + metadata + content + help).
- `Cargo.toml` — Dependencies and CLI binary name (`oox`).
- `data/sample.pptx` — Default sample file.

## Tech Stack

Rust, ratatui, crossterm, tui-tree-widget, edtui (Vim-mode editor with syntax highlighting), zip (Deflate feature only), quick-xml, image, and ratatui-image.

## Build & Test

```bash
cargo build
cargo test
cargo run -- data/sample.pptx
```

## Git Safety

- Do not commit, push, or create pull requests unless the user explicitly asks for it.
- Leave implementation changes uncommitted by default.
- Before any requested commit, inspect `git status` and review the diff.

## Conventions

- Keep the TUI state in `App`; keep rendering in `ui.rs`; keep input handling in `main.rs`.
- Use `io::Result` for app-level errors. Avoid panics in production paths — replace `todo!()` and `.unwrap()` with proper error handling.
- Preserve the current widget focus model (`Tree` ↔ `TextArea`) and quit guards (only quit from tree or editor Normal mode).
- OOXML files are ZIP archives. Use the `zip` crate with the configured Deflate feature for container access and `quick-xml` for XML formatting.
- Keep optional dependency features minimal: `edtui` is configured without default features but with syntax highlighting; `zip` is configured without default features and with Deflate support.
- Keep image decoding and terminal rendering aligned: supported extensions and `image::ImageFormat` mappings belong together in `src/app.rs`.

## Common Tasks

- **Add new keybindings** → `src/keybindings.rs` (`Action` enum + `help_sections`), dispatch in `src/main.rs` (`run_app`); consider editor mode guards.
- **Change layout/styling** → `src/ui.rs` (and `src/layout.rs` for geometry shared with mouse hit testing).
- **Add OOXML semantics** → extend `src/summary/`; consider higher-level OOXML crates before hand-rolling parsers.
- **Switch XML engine** → `pretty_print_xml` in `src/preview.rs` is the only XML formatting site.
- **Change image support** → update `is_image_name`/`image_format` in `src/package.rs` and the `image` features in `Cargo.toml` together.
