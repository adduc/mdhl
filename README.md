# mdhl

Reads Markdown and prints an ANSI-color highlighted version to stdout.
Parses with `pulldown-cmark`, renders to the terminal — no HTML, no pager.

## Build

Needs Rust 1.95 or newer.

```sh
make        # build a release binary (cargo build --release)
make test   # run the tests
```

### Installing

`make install` copies the binary to `~/.local/bin`; `make uninstall`
removes it. For a system-wide install, set `PREFIX`, and build first so
cargo doesn't run as root:

```sh
make
sudo make install PREFIX=/usr/local
```

`DESTDIR` is supported for staged installs when packaging.

### Prebuilt binaries

Each [release](https://github.com/adduc/mdhl/releases) has:

- RPMs for Fedora 44 and 45 (x86_64). Install the one matching your
  release with dnf:

  ```sh
  sudo dnf install ./mdhl-*.fc$(rpm -E %fedora).x86_64.rpm
  ```

- A tarball with a statically linked binary for any other Linux
  distribution, for x86_64 and aarch64. It needs no libraries, so it runs
  on glibc and musl (Alpine) systems alike:

  ```sh
  v=X.Y.Z  # the release you downloaded
  tar -xzf mdhl-$v-$(uname -m)-linux.tar.gz
  install -Dm755 mdhl-$v-$(uname -m)-linux/mdhl ~/.local/bin/mdhl
  ```

`SHA256SUMS` lists the checksums of every file (`sha256sum -c --ignore-missing SHA256SUMS`).

To build the tarball yourself, add the musl target and run `make dist`. It
lands in `target/dist/`. Set `RUST_TARGET` to build for another
architecture, e.g. `RUST_TARGET=aarch64-unknown-linux-musl`.

```sh
rustup target add $(uname -m)-unknown-linux-musl
make dist
```

### Fedora RPM

To build the RPMs yourself, install the packaging tools and run `make rpm`.
The source tarball comes from `git archive`, so only committed changes are
included. The RPMs land in `target/rpmbuild/`.

```sh
sudo dnf install rpm-build cargo-rpm-macros
make rpm
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

## Releasing

1. Bump the version in `Cargo.toml` and in `Version:` of
   `packaging/mdhl.spec`, and add a `%changelog` entry to the spec for
   the new version. `packaging/check-version.sh` (also run on every pull
   request) checks that all three agree.

   For a pre-release, RPM writes the suffix with `~` instead of `-`: a
   Cargo version of `0.2.0-rc.1` is `Version: 0.2.0~rc.1` in the spec and
   in its changelog entry, and the tag is `v0.2.0-rc.1`. It's published as
   a GitHub pre-release.
2. Merge to `main`, then tag and push:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

The Release workflow builds the RPMs in Fedora 44 and 45 containers and
the static tarballs for x86_64 and aarch64, then attaches them all, with a
`SHA256SUMS` file, to a GitHub release for the tag. It stops if the tag
doesn't match the version. Every pull request runs the same builds and
assembles the release files, checksums included, stopping short of
publishing.

## License

GPL-3.0-or-later. See `LICENSE`.
