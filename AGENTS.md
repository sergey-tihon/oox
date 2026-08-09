# AGENTS.md

## Project

`ooxml-tui` is a Rust terminal app for inspecting Office Open XML documents (`.pptx`, `.docx`, `.xlsx`). It opens the OOXML ZIP package, lists its parts in a tree, previews XML content with syntax highlighting, and displays common embedded images.

## Architecture

- `src/main.rs` — Entry point, terminal setup/teardown, event loop.
- `src/app.rs` — `App` state: zip tree, tree selection, editor state, file loading, XML pretty-printing.
- `src/ui.rs` — Ratatui layout and widgets (tree + editor).
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

- **Add new keybindings** → `src/main.rs` (`run_app`), consider editor mode guards.
- **Change layout/styling** → `src/ui.rs`.
- **Add OOXML semantics** → extend `src/app.rs`; consider higher-level OOXML crates before hand-rolling parsers.
- **Switch XML engine** → `App::pretty_print_xml` in `src/app.rs` is the only XML formatting site.
- **Change image support** → update `is_image`, `image_format`, and the `image` features in `Cargo.toml` together.
