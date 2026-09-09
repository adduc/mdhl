# mdhl

Reads Markdown and prints an ANSI-color highlighted version to stdout.
Parses with `pulldown-cmark`, renders to the terminal — no HTML, no pager.

## Build

```sh
cargo build --release
```

## Usage

```sh
mdhl [OPTIONS] [FILE]
```

Reads Markdown from `FILE`, or from stdin when omitted, and writes the
highlighted rendering to stdout.

```sh
mdhl README.md
cat notes.md | mdhl
mdhl --color=never doc.md > doc.rendered.txt
mdhl sample.md --width 80
```

Exit codes: `0` on success, `2` on usage errors, `141` when the reader
(e.g. `| head`) goes away — like `cat`.

## Options

| Option              | Meaning                                                             |
| ------------------- | ------------------------------------------------------------------- |
| `--color[=WHEN]`    | Force colors. `WHEN`: `always` (default for this flag), `never`, `auto` (TTY with no `NO_COLOR`) |
| `--no-color`        | Disable ANSI colors (same as `--color=never`)                       |
| `-w, --width N`     | Wrap output at N display columns; no wrapping by default            |
| `-h, --help`        | Show help                                                           |
| `-V, --version`     | Show version                                                        |

### Color policy

ANSI escapes are emitted when stdout is a TTY and `NO_COLOR` is unset.
Piping the output or setting `NO_COLOR` turns colors off.
`--color` / `--color=always` forces them on, even when piped.

### Width wrapping (`--width N`)

- Paragraphs, code lines, and link URLs word-wrap to at most N display
  columns. Continuation lines re-emit their gutter prefixes (`> `, `• `,
  `│ `, list indents), so wrapped lines stay aligned inside their block.
- Tables shrink-fit: columns are narrowed (cell text wraps) so the whole
  table, borders included, fits in N columns minus the active gutter.
  Columns with content never shrink below 1 char; if N is too small the
  table overflows, but rows stay aligned.
- Horizontal rules (`---`) span the full available line — N minus the
  active gutter — matching shrink-fit tables that fill the full width:

  ```text
  $ mdhl --width 40
  ...
  │ 1          │          2 │     3      │
  ...
  ────────────────────────────────────────
  ```

  A table narrower than N stays at its natural width; the rule still
  spans the full line.

## Rendering

ATX and setext headings, bold / italic / strikethrough, inline code,
fenced and indented code blocks, nested blockquotes, ordered / unordered /
task lists, tables with column alignment, inline / reference / autolink /
`mailto:` links, inline and display math, footnotes, and raw HTML blocks.

## Development

Single-file CLI in `src/main.rs`. `sample.md` exercises every
rendering feature.

```sh
cargo test
cargo run -- sample.md --width 80
```
