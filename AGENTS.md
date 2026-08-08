# AGENTS.md

## Project

`ooxml-tui` is a Rust terminal app for inspecting Office Open XML documents (`.pptx`, `.docx`, `.xlsx`). It opens the OOXML zip package, lists its parts in a tree, and previews XML content with syntax highlighting.

## Architecture

- `src/main.rs` — Entry point, terminal setup/teardown, event loop.
- `src/app.rs` — `App` state: zip tree, tree selection, editor state, file loading, XML pretty-printing.
- `src/ui.rs` — Ratatui layout and widgets (tree + editor).
- `Cargo.toml` — Dependencies and binary name (`ooxml`).
- `data/sample.pptx` — Default sample file.

## Tech Stack

Rust, ratatui, crossterm, tui-tree-widget, edtui (Vim-mode editor), zip, xml-rs.

## Build & Test

```bash
cargo build
cargo test
cargo run -- data/sample.pptx
```

## Conventions

- Keep the TUI state in `App`; keep rendering in `ui.rs`; keep input handling in `main.rs`.
- Use `io::Result` for app-level errors. Avoid panics in production paths — replace `todo!()` and `.unwrap()` with proper error handling.
- Preserve the current widget focus model (`Tree` ↔ `TextArea`) and quit guards (only quit from tree or editor Normal mode).
- OOXML files are zip archives. Use the `zip` crate for container access and `xml-rs` for XML formatting.

## Common Tasks

- **Add new keybindings** → `src/main.rs` (`run_app`), consider editor mode guards.
- **Change layout/styling** → `src/ui.rs`.
- **Add OOXML semantics** → extend `src/app.rs`; consider higher-level OOXML crates before hand-rolling parsers.
- **Switch XML engine** → `App::pretty_print_xml` in `src/app.rs` is the only XML formatting site.
