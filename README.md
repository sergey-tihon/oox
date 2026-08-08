# ooxml-tui

A terminal user interface for inspecting Office Open XML (OOXML) documents such as `.pptx`, `.docx`, and `.xlsx`.

## Features

- **Tree inspector** — Browse the parts inside an OOXML package as a tree.
- **XML viewer** — View selected XML parts with syntax highlighting.
- **Vim-like navigation** — Move through files with `j`/`k` and the editor with Vim bindings.

## Tech Stack

- [Rust](https://www.rust-lang.org/)
- [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) — cross-platform TUI
- [tui-tree-widget](https://github.com/EdJoPaTo/tui-tree-widget) — tree widget
- [edtui](https://github.com/vduggen/EdTui) — editor widget with Vim mode and syntax highlighting
- [zip](https://github.com/zip-rs/zip) — read OOXML zip containers
- [xml-rs](https://github.com/kornelski/xml-rs) — XML parsing and pretty-printing

## Usage

```bash
# Inspect a specific OOXML file
cargo run -- path/to/document.pptx

# Inspect the bundled sample file
cargo run
```

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down in the tree |
| `k` / `↑` | Move up in the tree |
| `Enter` | Toggle directory / load file content |
| `Tab` | Switch focus between tree and content |
| `q` | Quit (only when editor is in Normal mode) |

## Development

```bash
cargo build
cargo test
cargo run -- data/sample.pptx
```

## License

See [LICENSE](LICENSE).
