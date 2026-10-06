# oox

[![Crates.io](https://img.shields.io/crates/v/oox-tui.svg)](https://crates.io/crates/oox-tui)
[![CI](https://github.com/sergey-tihon/oox/actions/workflows/ci.yml/badge.svg)](https://github.com/sergey-tihon/oox/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/oox-tui.svg)](#license)

`oox` is a terminal UI for looking inside Office Open XML files (`.docx`, `.xlsx`, `.pptx`).
It shows the ZIP package as a tree of parts, pretty-prints the XML, previews embedded images,
and lets you search, compare, and edit parts without unzipping anything.

![oox browsing a PowerPoint package](https://raw.githubusercontent.com/sergey-tihon/oox/main/docs/demo.gif)

## Install

```bash
cargo install oox-tui --locked
```

The crate is called `oox-tui`; it installs a command called `oox`. Requires Rust 1.90 or newer.

## Usage

```bash
oox report.docx              # inspect a package
oox before.xlsx after.xlsx   # compare two packages part by part
```

## Features

- Browse every part in the package. XML and JSON are pretty-printed and syntax-highlighted.
- Preview embedded PNG, JPEG, GIF, BMP, and WebP images. Other binary parts get a hex dump or a metadata summary.
- Follow relationships: jump from an `r:id` in the XML to the part it points to, and back.
- Search part names (`/`) or part contents (`Ctrl-f`).
- Check package structure. Dangling relationships, parts without a content type, orphan parts, duplicate relationship ids, and missing required parts are flagged with `⚠`.
- Summarize the document: slides, headings, tables, sheets, and formulas (`s`).
- Compare two files. Parts are marked added (`+`), removed (`-`), or changed (`~`), and the diff ignores attribute order and whitespace-only changes.
- Edit XML, JSON, and text parts in place or in `$EDITOR`, then save a copy of the package (`<name>.edited.<ext>` by default). Untouched parts are copied byte for byte.
- Extract a part to a file, open it in `$PAGER`, or copy it to the clipboard.

Untrusted files are safe to open: archive size, entry count, part reads, and image dimensions are capped, and embedded content is never executed.

## Keys

| Key                  | Action                                  |
| -------------------- | --------------------------------------- |
| `j` `k`, `Up` `Down` | Move in the tree                 |
| `Enter`              | Open part / expand folder               |
| `Tab`, `1` `2` `3`   | Switch panel                            |
| `/`, `Ctrl-f`        | Search names, search contents           |
| `n` `N`              | Next / previous match                   |
| `Ctrl-g`, `Alt-Left` | Follow relationship, go back            |
| `i` `I`              | Next / previous structure issue         |
| `s`, `d`             | Toggle summary, toggle metadata panel   |
| `u`                  | Hide unchanged parts (compare mode)     |
| `x`, `o`, `y`        | Extract, open externally, copy          |
| `Ctrl-s`, `Ctrl-e`   | Save package, edit part in `$EDITOR`    |
| `R`                  | Revert unsaved edits to the selected part |
| `?`                  | Show all keys                           |
| `q`                  | Quit                                    |

The mouse selects, scrolls, and follows relationship links. The content pane uses Vim keys by default.

## Configuration

`oox --generate-config` writes a commented config file to:

- Linux: `$XDG_CONFIG_HOME/oox/config.toml` (default `~/.config/oox/config.toml` when `XDG_CONFIG_HOME` is unset)
- macOS: `~/Library/Application Support/oox/config.toml`
- Windows: `%APPDATA%\oox\config.toml`

It is loaded automatically; `--config <path>` loads a different one. Every key can be rebound, and the editor can be switched to Emacs mode:

```toml
[editor]
mode = "emacs" # default: "vim"

[keybindings]
help = ["?", "F1"]
move_down = ["j", "Down"]
```

## Image previews

Images render in any terminal using Unicode half blocks. Terminals with Kitty graphics, iTerm2, or Sixel support (Ghostty, Kitty, WezTerm, iTerm2) can use native graphics protocols for sharper previews; images remain fitted to the content pane.

## Troubleshooting

`OOX_DEBUG=1 oox file.pptx` logs key and terminal events to `/tmp/oox-debug.log`.

## Contributing

Issues and pull requests are welcome. From a checkout, `cargo run -- data/sample.pptx` opens the bundled sample. CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
