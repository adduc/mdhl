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
| `--table-width N`   | Fit tables in N display columns, overriding `--width` for tables    |
| `--code-width N`    | Wrap code blocks at N display columns, overriding `--width` for code |
| `--no-width`        | Remove the `--width` limit (e.g. one set in the config)             |
| `--no-table-width`  | Don't limit table width, even when `--width` is set                 |
| `--no-code-width`   | Don't wrap code blocks, even when `--width` is set                  |
| `--no-config`       | Ignore the config file                                              |
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
  active gutter — so with `--width` alone they line up with shrink-fit
  tables that fill the full width:

  ```text
  $ mdhl --width 40
  ...
  │ 1          │          2 │     3      │
  ...
  ────────────────────────────────────────
  ```

  A table narrower than N stays at its natural width; the rule still
  spans the full line. Rules always follow `--width` (40 columns without
  it), not `--table-width`, so with a separate table width the two no
  longer line up.

### Table width (`--table-width N`)

Sets the shrink-fit budget for tables independently of `--width`. It can
be wider or narrower than `--width`, and works without it:

```sh
mdhl doc.md --width 80 --table-width 120   # prose at 80, tables up to 120
mdhl doc.md --width 100 --table-width 60   # keep tables compact
mdhl doc.md --table-width 80               # only tables are constrained
```

Gutters still count against the budget. Table rows are never wrapped by
`--width`, so a table wider than `--width` keeps its borders intact.
Horizontal rules keep following `--width`.

### Code width (`--code-width N`)

Sets the wrap width for fenced and indented code blocks (and their
language caption) independently of `--width`, and falls back to `--width`
when unset. Like `--table-width`, it can be wider or narrower than
`--width`, and gutters count against it:

```sh
mdhl doc.md --width 80 --code-width 120   # don't break long code lines at 80
mdhl doc.md --code-width 100              # wrap only code blocks
```

Inline code spans are prose and follow `--width`.

## Config file

Default options are read from the first usable one of these files:

1. `$MDHL_CONFIG` — if set, the only file tried, and it must exist
2. `$XDG_CONFIG_HOME/mdhl/config` (default `~/.config/mdhl/config`)
3. `/etc/mdhl/config` — system-wide fallback

A missing default file is skipped silently; a broken one is warned about
and skipped (see below). So a typo in your user config means
`/etc/mdhl/config`, if it exists, applies instead.

The file holds options as on the command line, separated by whitespace or
newlines. There is no shell quoting: values can't contain spaces. A word
starting with `#` begins a comment that runs to the end of the line, so
both whole-line and trailing comments work (`-w 80  # wrap prose`).

```text
# ~/.config/mdhl/config
-w 80
--table-width 120
--code-width 120
--color
```

Command-line options are applied after the config, so each one wins over
the same option in the config: `--width 60` replaces the configured width,
`--no-width` / `--no-table-width` / `--no-code-width` remove a configured
limit, and `--color=auto` / `--no-color` undo a configured `--color`.
`--no-config` skips the file entirely.

`--width` only replaces `--width`. A `--table-width` or `--code-width` set
in the config still applies, so with the config above, `mdhl -w 60 doc.md`
wraps prose at 60 but keeps tables and code at 120. Pass `--table-width` /
`--code-width` (or `--no-table-width` / `--no-code-width`) on the command
line to change those too.

A config may only set options: `FILE`, `--help`, `--version`, and
`--no-config` are rejected. A missing config file is silently skipped. Any
other problem with one of the default files — it can't be read, isn't a
regular file, or contains an invalid option — prints a warning naming the
file, and that file is ignored (the next one in the list is tried). An
explicit `$MDHL_CONFIG` is stricter: any such problem, including a missing
file, is an error (exit `2`). Command-line errors and
`--help` are reported before the config is read, so a broken config
never hides them.

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
