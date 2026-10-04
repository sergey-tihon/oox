# oox

A terminal user interface for inspecting Office Open XML (OOXML) documents such as `.pptx`, `.docx`, and `.xlsx`.

## Features

- **Tree inspector** — Browse the parts inside an OOXML package as a tree.
- **XML viewer** — View selected XML parts with syntax highlighting and indentation.
- **Image preview** — Preview common embedded PNG, JPEG, GIF, BMP, and WebP images.
- **Raw file previews** — View plain text and JSON, inspect `.bin` files as hex, and see metadata for binary media, fonts, and OLE parts.
- **Content search** — Grep package-part contents in the background and filter the tree to matching parts.
- **Export parts** — Extract a part to a file, open it in `$PAGER`/`$EDITOR`, or copy its pretty-printed content to the clipboard (OSC 52).
- **Inline relationship jumps** — Follow an `r:id`/`r:embed` reference inside the XML preview with `Ctrl-g` or a mouse click: internal targets open the referenced part (images preview directly), external URLs are reported in the status bar, and `Alt-Left` returns to where you jumped from.
- **Document summaries** — Press `s` to inspect slide, paragraph, heading, table, sheet, cell, and formula summaries for PowerPoint, Word, and Excel packages; linked part paths navigate back to the tree.
- **Package integrity** — Phase-1 OPC checks (dangling relationship targets, parts without a content type, orphan parts, duplicate relationship ids, missing required parts) are listed in the metadata panel, marked with `⚠` in the tree, and reachable with `i`/`I`.
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
cargo install oox-tui
```

Or install the latest source version:

```bash
git clone https://github.com/sergey-tihon/oox.git
cd oox
cargo install --path .
```

## Usage

```bash
# Inspect a specific OOXML file
oox path/to/document.pptx

# Compare two packages part by part
# The tree marks parts as added (`+`), removed (`-`), or changed (`~`); the
# content pane shows a unified diff of normalized XML for the selected part.
oox before.docx after.docx

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
| `e`       | Extract the selected part to a file         |
| `o`       | Open the selected part in `$PAGER`/`$EDITOR` |
| `y`       | Copy pretty-printed content to the clipboard |
| `/`       | Search and live-filter package paths          |
| `Ctrl-f`  | Search part contents in the background       |
| `n` / `N` | Select the next / previous search match     |
| `i` / `I` | Jump to the next / previous part with a package issue |
| `Ctrl-g`  | Follow the `r:id`/`r:embed` reference under the content cursor |
| `Esc`     | Cancel search / clear the applied filter    |
| `Enter`   | Toggle directory / preview file content     |
| `1` / `2` / `3` | Focus tree / metadata / content panels  |
| `Tab`     | Cycle tree / metadata / content focus       |
| `?` / `F1` | Show the help screen                        |
| `d`       | Toggle the metadata panel                   |
| `s`       | Toggle the document-specific summary        |
| Mouse       | Select/expand tree; scroll tree/metadata; click relationship targets |
| `q`       | Quit from tree / Vim normal mode             |
| `Ctrl-q`  | Quit from Emacs editor                       |
| `Alt-Left` / `Alt-Right` | Previous / next opened part       |

## Package safety and loading

Package metadata uses one canonical normalized path model. ZIP entries with traversal-like names, colliding normalized paths, and malformed relationships/content types are retained as structured diagnostics and are not allowed to overwrite another part. Archives exceeding 100,000 entries or 256 MiB of declared uncompressed content are rejected as failed opens; individual reads are bounded while data is decompressed (32 MiB per part and 4 MiB for indexing metadata), and declared ZIP sizes are not trusted as a substitute for the streaming limit. Hex previews are capped at 1 MiB and images are limited to 8192×8192 and 16 million pixels.

Archive indexing, document summaries, and selected-part preview work run on a bounded background worker after the loading screen is entered. Messages contain owned package metadata/preview payloads; request IDs and selected canonical paths discard stale results. The UI remains the sole owner of editor state and creates ratatui image protocols on the UI thread. Loading and malformed/limited-part failures are shown in the status area rather than panicking. Terminal mode is restored on normal exits and unwinding errors on a best-effort basis.

The initial package is not indexed synchronously: the tree and summary appear when the worker finishes, and tree/content actions are ignored while loading. Summary XML parser failures are retained as structured diagnostics in package metadata instead of displaying a partial summary; summary output and extracted item/text collections are bounded to prevent oversized documents from consuming unbounded memory.

Phase-1 package integrity is structural only (no schema validation): internal relationship targets that do not resolve (URI fragments are ignored), parts with no content type, parts unreachable from `/_rels/.rels`, duplicate relationship ids within one `.rels` part, and missing required parts (`[Content_Types].xml`, root rels, the `officeDocument` main part). Explicitly external relationships are never dangling; relationship parts (`_rels/.rels` and `.../_rels/*.rels`) are exempt from the reachability rule because OPC resolves them implicitly rather than through relationships, but they still need a content type, so a manifest without a `rels` default is reported; OPC-reserved bracket names such as `[Content_Types].xml` and `[trash]` are exempt from both rules.

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
