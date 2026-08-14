# oox

A terminal user interface for inspecting Office Open XML (OOXML) documents such as `.pptx`, `.docx`, and `.xlsx`.

## Features

- **Tree inspector** — Browse the parts inside an OOXML package as a tree.
- **XML viewer** — View selected XML parts with syntax highlighting and indentation.
- **Image preview** — Preview common embedded PNG, JPEG, GIF, BMP, and WebP images.
- **Raw file previews** — View plain text and JSON, inspect `.bin` files as hex, and see metadata for binary media, fonts, and OLE parts.
- **Document summaries** — Press `s` to inspect slide, paragraph, heading, table, sheet, cell, and formula summaries for PowerPoint, Word, and Excel packages; linked part paths navigate back to the tree.
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
cargo run -- data/sample.pptx

# Show command-line help and version
oox --help
oox --version
```

## Keybindings

| Key       | Action                                    |
| --------- | ----------------------------------------- |
| `j` / `↓` | Move down in the tree                       |
| `k` / `↑` | Move up in the tree                         |
| `Ctrl-d` / `Ctrl-u` | Scroll down / up in the tree       |
| `g` / `G` | Select the first / last visible item        |
| `E` / `C` | Expand / collapse all tree nodes              |
| `/`       | Search and live-filter package paths          |
| `n` / `N` | Select the next / previous search match     |
| `Esc`     | Cancel search / clear the applied filter    |
| `Enter`   | Toggle directory / preview file content     |
| `1` / `2` / `3` | Focus tree / metadata / content panels  |
| `Tab`     | Cycle tree / metadata / content focus       |
| `?` / `F1` | Show the help screen                        |
| `d`       | Toggle the metadata panel                   |
| `s`       | Toggle the document-specific summary        |
| Mouse       | Select/expand tree; scroll tree/metadata; click relationship targets |
| `Esc`     | Cancel search or close help (clears filter) |
| `q`       | Quit from tree / Vim normal mode             |
| `Ctrl-q`  | Quit from Emacs editor                       |
| `Alt-Left` / `Alt-Right` | Previous / next opened part       |

## Package safety and loading

Package metadata uses one canonical normalized path model. ZIP entries with traversal-like names, colliding normalized paths, and malformed relationships/content types are retained as structured diagnostics and are not allowed to overwrite another part. Archives exceeding 100,000 entries or 256 MiB of declared uncompressed content are rejected as failed opens; individual reads are bounded while data is decompressed (32 MiB per part and 4 MiB for indexing metadata), and declared ZIP sizes are not trusted as a substitute for the streaming limit. Hex previews are capped at 1 MiB and images are limited to 8192×8192 and 16 million pixels.

Archive indexing, document summaries, and selected-part preview work run on a bounded background worker after the loading screen is entered. Messages contain owned package metadata/preview payloads; request IDs and selected canonical paths discard stale results. The UI remains the sole owner of editor state and creates ratatui image protocols on the UI thread. Loading and malformed/limited-part failures are shown in the status area rather than panicking. Terminal mode is restored on normal exits and unwinding errors on a best-effort basis.

The initial package is not indexed synchronously: the tree and summary appear when the worker finishes, and tree/content actions are ignored while loading. Summary XML parser failures are retained as structured diagnostics in package metadata instead of displaying a partial summary; summary output and extracted item/text collections are bounded to prevent oversized documents from consuming unbounded memory.

## Terminal image support

Image previews work in all terminals using a Unicode half-block fallback. For sharper previews, use a terminal with Kitty graphics, iTerm2, or Sixel support, such as Ghostty, Kitty, WezTerm, or iTerm2.

## Configuration

Generate a documented configuration file in the system config directory:

```bash
oox --generate-config
```

Run with the automatically discovered configuration:

```bash
oox data/sample.pptx
```

Use `--config` only when you want to load a different file:

```bash
oox --config /path/to/config.toml data/sample.pptx
```

The generated file contains editor mode and application keybindings. Each binding
is an array of alternative single-key shortcuts:

```toml
[editor]
mode = "vim" # or "emacs"

[keybindings]
help = ["?", "F1"]
move_down = ["j", "Down"]
show_metadata = ["d"]
show_summary = ["s"]
```

The keybinding help screen is generated from the active configuration.

## Debugging

Enable key and event logging while troubleshooting terminal input:

```bash
OOX_DEBUG=1 cargo run -- data/sample.pptx
```

Debug messages are written to `/tmp/oox-debug.log`, so they do not corrupt the TUI:

```bash
tail -f /tmp/oox-debug.log
```

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
