# PixelPad

A tiny, pixel-styled terminal text editor written in Rust, with a sandboxed
Lua plugin system. Built on [crossterm](https://github.com/crossterm-rs/crossterm)
for terminal rendering and [mlua](https://github.com/mlua-rs/mlua) for plugins.

This is a Rust port of an original Python (curses) implementation.

```
████████████████████████████████████████████
█  █ PIXELPAD █                             █
█ 1 fn main() {                             █
█ 2     println!("hello, pixelpad");        █
█ 3 }                                       █
█                                           █
█ scratch.rs — Ln 2, Col 32  Syntax:on      █
████████████████████████████████████████████
```

## Features

- Modal-free, always-insert editing (no vim-style modes) with familiar
  Ctrl-key shortcuts
- Undo/redo with coalesced typing checkpoints (a burst of typing is one
  undo step, not one per keystroke)
- Incremental search with wraparound
- Basic syntax highlighting (keywords, strings, numbers, comments) covering
  several C-like, Python-like, and JS-like languages out of the box
- A sandboxed Lua plugin system: drop a `.lua` file in `plugins/` next to
  the binary and it shows up in the plugin menu, with an optional hotkey
- Runs anywhere crossterm does (Linux, macOS, Windows — including plain
  Windows consoles, not just ANSI-aware terminals)

## Installation

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (recent stable toolchain)
- A C compiler (`cc`/`gcc`/`clang`, or MSVC on Windows) — required to build
  the vendored Lua 5.4 that `mlua` compiles from source

### Build from source

```bash
git clone https://github.com/YOUR_USERNAME/pixelpad.git
cd pixelpad
cargo build --release
```

The binary is at `target/release/pixelpad`. Optionally install it onto your
`PATH`:

```bash
cargo install --path .
```

## Usage

```bash
pixelpad [file]
```

If `file` doesn't exist yet, PixelPad starts with an empty buffer and that
path pre-filled as the save target.

### Controls

| Key                | Action                                    |
|--------------------|--------------------------------------------|
| `Ctrl-S`           | Save                                      |
| `Ctrl-A`           | Save As                                   |
| `Ctrl-O`           | Open (prompts for a path)                 |
| `Ctrl-N`           | New file                                  |
| `Ctrl-F`           | Find (search forward, wraps around)       |
| `Ctrl-K`           | Cut current line (into clipboard)         |
| `Ctrl-C`           | Copy current line (into clipboard)        |
| `Ctrl-U`           | Paste clipboard (as a line above cursor)  |
| `Ctrl-Z` / `Ctrl-Y`| Undo / Redo                               |
| `Ctrl-T`           | Toggle syntax highlighting                |
| `Ctrl-P`           | Open the plugin menu                      |
| `Ctrl-G`           | Help overlay                              |
| `Ctrl-Q`           | Quit (confirms if there are unsaved changes) |
| Arrow keys         | Move cursor                               |
| `Home` / `End`     | Line start / end                          |
| `PageUp`/`PageDown`| Scroll                                    |
| `Backspace`/`Delete`| Remove text                              |

## Plugins

PixelPad loads every `*.lua` file in a `plugins/` directory next to the
executable at startup. Each plugin gets its own sandboxed Lua VM — plugins
can't touch the filesystem, spawn processes, or see each other's state.
A plugin that fails to load only disables itself; it never stops the editor
from starting.

A minimal plugin looks like this:

```lua
plugin = {
    name = "Uppercase Line",         -- optional, defaults to the filename
    description = "Uppercases the current line", -- optional
    hotkey = "ctrl-w",               -- optional; ignored if it collides
                                      -- with a built-in Ctrl shortcut
}

function plugin.run()
    local row = pixelpad:get_cursor()
    pixelpad:set_line(row, pixelpad:get_line(row):upper())
end
```

See [`plugins/word_count.lua`](plugins/word_count.lua) for a complete
example, and the table below for the full API surface exposed to plugins as
the global `pixelpad` object. Buffer lines and columns are **1-indexed** on
the Lua side, matching Lua convention (they're 0-indexed internally in Rust).

| Method                          | Description                                      |
|----------------------------------|--------------------------------------------------|
| `pixelpad:get_line_count()`      | Number of lines in the buffer                    |
| `pixelpad:get_line(i)`           | Contents of line `i`                             |
| `pixelpad:set_line(i, text)`     | Replace line `i`                                 |
| `pixelpad:insert_line(i, text)`  | Insert a new line before position `i`            |
| `pixelpad:remove_line(i)`        | Remove line `i` (buffer always keeps ≥1 line)    |
| `pixelpad:get_lines()`           | All lines as a 1-indexed table                   |
| `pixelpad:set_lines(table)`      | Replace the whole buffer                         |
| `pixelpad:get_cursor()`          | Returns `row, col`                               |
| `pixelpad:set_cursor(row, col)`  | Move the cursor (clamped to the buffer)          |
| `pixelpad:insert_text(text)`     | Insert text at the cursor (`\n` starts a line)   |
| `pixelpad:filename()`            | Current file path                                |
| `pixelpad:filetype()`            | Lowercased file extension, no dot                |
| `pixelpad:message(text)`         | Show a status-bar message for 3 seconds          |
| `pixelpad:prompt(text)`          | Blocking status-bar input prompt; returns a string |

Runaway plugins (e.g. an accidental infinite loop) are stopped automatically
after a few seconds rather than hanging the editor.

## Project layout

```
src/
├── main.rs    entry point, event loop, keybinding dispatch
├── editor.rs  buffer state and edit operations (no terminal/Lua knowledge)
├── ui.rs      crossterm rendering, prompts, overlays
├── syntax.rs  single-pass tokenizer for basic syntax highlighting
└── plugin.rs  Lua plugin loading, sandboxing, and the `pixelpad` API
plugins/
└── word_count.lua   example plugin
```

`editor.rs` deliberately has no knowledge of the terminal or of Lua, so the
buffer/undo logic can be tested in isolation from rendering and plugins.

## Development

```bash
cargo build          # debug build
cargo test           # run the test suite
cargo run -- [file]  # run without installing
```

## Contributing

Issues and pull requests are welcome. For anything non-trivial, please open
an issue first to discuss the change.

## License

Licensed under the [MIT License](LICENSE).
