# oox

A terminal user interface for inspecting Office Open XML (OOXML) documents such as `.pptx`, `.docx`, and `.xlsx`.

## Features

- **Tree inspector** — Browse the parts inside an OOXML package as a tree.
- **XML viewer** — View selected XML parts with syntax highlighting and indentation.
- **Image preview** — Preview common embedded PNG, JPEG, GIF, BMP, and WebP images.
- **Vim-like navigation** — Move through files with `j`/`k` and the editor with Vim bindings.

## Tech Stack

- [Rust](https://www.rust-lang.org/)
- [ratatui](https://github.com/ratatui/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) — cross-platform TUI
- [tui-tree-widget](https://github.com/EdJoPaTo/tui-tree-widget) — tree widget
- [edtui](https://github.com/preiter93/edtui) — editor widget with Vim mode and syntax highlighting
- [zip](https://github.com/zip-rs/zip2) — read OOXML ZIP containers (Deflate support)
- [quick-xml](https://github.com/tafia/quick-xml) — XML parsing and pretty-printing
- [image](https://github.com/image-rs/image) + [ratatui-image](https://github.com/EdJoPaTo/ratatui-image) — decode and render embedded images

## Installation

Install the published binary from crates.io:

```bash
cargo install oox
```

Or install the latest source version:

```bash
git clone https://github.com/sergey-tihon/ooxml-tui.git
cd ooxml-tui
cargo install --path .
```

## Usage

```bash
# Inspect a specific OOXML file
oox path/to/document.pptx

# Inspect the bundled sample file from a source checkout
cargo run

# Show command-line help and version
oox --help
oox --version
```

## Keybindings

| Key       | Action                                    |
| --------- | ----------------------------------------- |
| `j` / `↓` | Move down in the tree                     |
| `k` / `↑` | Move up in the tree                       |
| `Enter`   | Toggle directory / load file content      |
| `Tab`     | Switch focus between tree and content     |
| `q`       | Quit (only when editor is in Normal mode) |

## Terminal image support

Image previews work in all terminals using a Unicode half-block fallback. For sharper previews, use a terminal with Kitty graphics, iTerm2, or Sixel support, such as Ghostty, Kitty, WezTerm, or iTerm2.

## Development

```bash
cargo fmt --all -- --check
cargo check --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo build --all-targets --locked
cargo package --locked
```

## License

See [LICENSE](LICENSE).
