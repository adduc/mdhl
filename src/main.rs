//! mdhl — read Markdown and print an ANSI-color highlighted version to stdout.
//!
//! Input: a positional `FILE`, or stdin when omitted.
//!
//! Color policy: ANSI escapes are emitted when stdout is a TTY, unless
//! `NO_COLOR` is set or `--no-color` is given; `--color` forces them on.

//! Width: `--width N` wraps all output to at most N display columns;
//! `--table-width N` and `--code-width N` override it for tables and code
//! blocks; `--no-width` & co. remove a limit.
//!
//! Config: default options come from the first existing file among
//! `$MDHL_CONFIG`, `$XDG_CONFIG_HOME/mdhl/config` and `/etc/mdhl/config`,
//! in command-line syntax; the command line overrides them.

use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ---------------------------------------------------------------------------
// Palette (256-color indices)
// ---------------------------------------------------------------------------

const C_BORDER: u8 = 240; // `#` markers, code-block border, table borders, rules, `>`
const C_DIM: u8 = 245; // urls, html, footnote labels
const C_QUOTE: u8 = 244; // blockquote text
const C_H1: u8 = 198; // bright pink
const C_H2: u8 = 75; // bright cyan
const C_H3: u8 = 253; // near white (h3..h6)
const C_BOLD: u8 = 220; // bright yellow
const C_ITALIC: u8 = 141; // purple
const C_CODE: u8 = 51; // cyan (inline code)
const C_CODEBLOCK: u8 = 250; // light gray (fenced/indented code text)
const C_LINK: u8 = 39; // blue
const C_BULLET: u8 = 42; // green (bullets, ordered numbers, task checkboxes)
const C_HEADER: u8 = 75; // table header text
const C_MATH: u8 = 229; // yellow

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    fg: Option<u8>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    dim: bool,
}

impl Style {
    fn sgr(self, color: bool) -> Option<String> {
        if !color {
            return None;
        }
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.strike {
            codes.push("9".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if let Some(fg) = self.fg {
            let code = format!("38;5;{fg}");
            codes.push(code);
        }
        if codes.is_empty() {
            None
        } else {
            Some(format!("\u{1b}[{}m", codes.join(";")))
        }
    }
}

const RESET: &str = "\u{1b}[0m";

fn plain_style(fg: u8) -> Style {
    Style {
        fg: Some(fg),
        ..Default::default()
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let fg = match level {
        HeadingLevel::H1 => C_H1,
        HeadingLevel::H2 => C_H2,
        _ => C_H3,
    };
    Style {
        bold: true,
        underline: level == HeadingLevel::H1,
        fg: Some(fg),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Gutters: per-line prefixes for quotes, list items, code blocks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum GutterKind {
    Quote,
    Item,
    Code,
}

struct Gutter {
    kind: GutterKind,
    /// Prefix for the first line, rendered with `first_style`.
    first: String,
    first_style: Option<u8>,
    /// Plain padding for continuation lines.
    cont: String,
    /// Whether the first line of this gutter's element has been emitted.
    started: bool,
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

struct Emitter<'w, W: Write> {
    w: &'w mut W,
    color: bool,
    gutters: Vec<Gutter>,
    style: Style,
    style_stack: Vec<Style>,
    /// Per list level: `Some((next_number, digit_width))` for ordered lists.
    ordered: Vec<Option<(u64, usize)>>,
    /// Depth in the tag tree (0 = top level).
    depth: usize,
    /// Next visible content must start a new display line.
    pending: bool,
    /// The current display line already has bytes written.
    line_has_content: bool,
    /// A top-level block just ended: insert a blank line before the next block.
    top_gap: bool,
    /// The top-level block being emitted so far produced visible content.
    block_content: bool,
    /// Link/image url to emit after the span closes.
    link_url: String,
    /// Whether the open span is rendered with `[...]` brackets.
    link_brackets: bool,
    /// Language tag of the current code block, not yet printed.
    code_lang: Option<String>,
    /// The code-block caption is being written: wrapped caption lines keep
    /// the caption's prefix (outer gutters only, no code border).
    in_caption: bool,
    /// A footnote-definition paragraph continues on its label line.
    after_label: bool,
    /// Output width limits (global, tables, code blocks).
    widths: Widths,
    /// Display column the next byte lands in (visible columns on the line).
    col: usize,
    /// Column at which the current line's content starts (after gutters).
    line_start_col: usize,
    /// Trailing spaces of the last run, held back until the line is known
    /// to continue; dropped when it breaks.
    pending_sp: usize,
}

impl<'w, W: Write> Emitter<'w, W> {
    fn new(w: &'w mut W, color: bool, base_style: Style, widths: Widths) -> Self {
        Self {
            w,
            color,
            gutters: Vec::new(),
            style: base_style,
            style_stack: Vec::new(),
            ordered: Vec::new(),
            depth: 0,
            pending: false,
            line_has_content: false,
            top_gap: false,
            block_content: false,
            link_url: String::new(),
            link_brackets: false,
            code_lang: None,
            in_caption: false,
            after_label: false,
            widths,
            col: 0,
            line_start_col: 0,
            pending_sp: 0,
        }
    }

    fn quote_depth(&self) -> usize {
        self.gutters
            .iter()
            .filter(|g| g.kind == GutterKind::Quote)
            .count()
    }

    fn in_code_block(&self) -> bool {
        self.gutters.iter().any(|g| g.kind == GutterKind::Code)
    }

    /// Total display width of the active gutter prefixes.
    fn gutter_width(&self) -> usize {
        self.gutters.iter().map(|g| g.cont.width()).sum()
    }

    /// Final style for inline content: dim inside blockquotes unless the
    /// inline context (code, link, emphasis, heading) already picked a color.
    fn text_style(&self, mut s: Style) -> Style {
        if self.quote_depth() > 0 && s.fg.is_none() {
            s.fg = Some(C_QUOTE);
            s.dim = true;
        }
        s
    }

    fn push_style(&mut self) {
        self.style_stack.push(self.style);
    }

    fn pop_style(&mut self) {
        if let Some(s) = self.style_stack.pop() {
            self.style = s;
        }
    }

    /// Start a new display line: close the current one (adding the blank-line
    /// gap after top-level blocks), then emit the active gutter prefixes.
    /// `new_block` re-opens quote and code gutters so each new paragraph or
    /// code line gets its own `>` / `│`.
    fn begin_line(&mut self, new_block: bool) {
        self.begin_line_impl(new_block, self.in_caption);
    }

    /// Line prefix for a code-block language caption: outer gutters only,
    /// the block's own border is left for the first code line.
    fn begin_caption(&mut self) {
        self.begin_line_impl(true, true);
    }

    fn begin_line_impl(&mut self, new_block: bool, skip_code_gutter: bool) {
        let mut gap = false;
        if self.line_has_content {
            let _ = self.w.write_all(b"\n");
            gap = self.top_gap;
            self.line_has_content = false;
            self.col = 0;
            self.pending_sp = 0;
        }
        self.top_gap = false;
        if gap {
            let _ = self.w.write_all(b"\n");
        }
        let skip_idx = if skip_code_gutter {
            self.gutters
                .iter()
                .rposition(|g| g.kind == GutterKind::Code)
        } else {
            None
        };
        let prefixes: Vec<(String, Option<Style>)> = self
            .gutters
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| Some(*i) != skip_idx)
            .map(|(_, g)| {
                if new_block && (g.kind == GutterKind::Quote || g.kind == GutterKind::Code) {
                    g.started = false;
                }
                if g.started {
                    (g.cont.clone(), None)
                } else {
                    g.started = true;
                    (g.first.clone(), g.first_style.map(plain_style))
                }
            })
            .collect();
        for (prefix, style) in prefixes {
            self.write_run(&prefix, style);
        }
        self.line_start_col = self.line_width();
    }

    /// Write one run of `text`; `style` `None` means "unstyled".
    fn write_run(&mut self, text: &str, style: Option<Style>) {
        if text.is_empty() {
            return;
        }
        // Same line continues: release held-back spaces; new line drops them.
        if self.line_has_content {
            self.flush_spaces();
        } else {
            self.pending_sp = 0;
        }
        let mut off = text.len();
        for (idx, c) in text.char_indices().rev() {
            if !c.is_whitespace() {
                break;
            }
            off = idx;
        }
        let trimmed = &text[..off];
        self.pending_sp = text.chars().count() - trimmed.chars().count();
        if trimmed.is_empty() {
            self.line_has_content = true;
            return;
        }
        let s = style.unwrap_or_default();
        let open = s.sgr(self.color);
        if let Some(o) = &open {
            let _ = self.w.write_all(o.as_bytes());
        }
        let _ = self.w.write_all(trimmed.as_bytes());
        if open.is_some() {
            let _ = self.w.write_all(RESET.as_bytes());
        }
        self.line_has_content = true;
        self.block_content = true;
        self.col += trimmed.width();
    }

    /// Write the held-back trailing spaces (the line continues).
    fn flush_spaces(&mut self) {
        if self.pending_sp == 0 {
            return;
        }
        let _ = self.w.write_all(&b" ".repeat(self.pending_sp));
        self.col += self.pending_sp;
        self.pending_sp = 0;
    }

    /// Visible width already on the current line, held spaces included.
    fn line_width(&self) -> usize {
        self.col + self.pending_sp
    }

    /// Emit text with the current (or overridden) inline style, honoring the
    /// pending newline and embedded line breaks.
    fn emit_text(&mut self, text: &str, style: Option<Style>) {
        if text.is_empty() {
            return;
        }
        if self.pending {
            self.begin_line(true);
            self.pending = false;
        }
        let s = style.unwrap_or_else(|| self.text_style(self.style));
        let segs: Vec<&str> = text.split('\n').collect();
        let mut first = true;
        for seg in segs {
            if seg.is_empty() {
                continue;
            }
            if !first || self.pending {
                self.begin_line(true);
                self.pending = false;
            }
            first = false;
            let _ = self.wrap(seg, Some(s));
        }
    }

    /// Write `text` as one run, wrapping at the global width.
    fn wrap(&mut self, text: &str, style: Option<Style>) {
        self.wrap_at(text, style, self.widths.all);
    }

    /// Write `text` as one run, wrapping at word boundaries when `width` is
    /// set. A token longer than a full line is hard-broken characterwise.
    fn wrap_at(&mut self, text: &str, style: Option<Style>, width: Option<usize>) {
        if text.is_empty() {
            return;
        }
        let width = match width {
            // Width that does not even fit the current gutters: leave alone.
            Some(w) if w > self.line_start_col => w,
            _ => {
                self.write_run(text, style);
                return;
            }
        };
        if self.line_width().saturating_add(text.width()) <= width {
            self.write_run(text, style);
            return;
        }
        if self.line_width() >= width {
            self.begin_line(false);
        }
        let first = width - self.line_width();
        let rest = width - self.line_start_col;
        let pc: Vec<char> = text.chars().collect();
        // If the first token misses the line's remainder but fits on a
        // fresh line, the wrap moves it there.
        let mut k = 0usize;
        while k < pc.len() && pc[k].is_whitespace() {
            k += 1;
        }
        let mut j = k;
        while j < pc.len() && !pc[j].is_whitespace() {
            j += 1;
        }
        let tok = width_of(&pc[k..j]);
        let first_moves = tok > first && tok <= rest;
        let mut first_chunk = true;
        for (a, b) in wrap_char_ranges(&pc, first, rest) {
            if first_chunk {
                if first_moves {
                    self.begin_line(false);
                }
                first_chunk = false;
            } else if self.line_width() > self.line_start_col {
                self.begin_line(false);
            }
            let chunk: String = pc[a..b].iter().collect();
            self.write_run(&chunk, style);
        }
    }

    /// Render the text of a code block with the `│` border gutter.
    /// The language caption and code lines wrap at `--code-width`, falling
    /// back to `--width`.
    fn emit_code(&mut self, text: &str) {
        let width = self.widths.code();
        if let Some(lang) = self.code_lang.take() {
            if !lang.is_empty() {
                self.begin_caption();
                self.in_caption = true;
                self.wrap_at(
                    &lang,
                    Some(Style {
                        italic: true,
                        fg: Some(C_ITALIC),
                        dim: true,
                        ..Default::default()
                    }),
                    width,
                );
                self.in_caption = false;
            }
        }
        let segs: Vec<&str> = text.split('\n').collect();
        let style = plain_style(C_CODEBLOCK);
        for (i, seg) in segs.iter().enumerate() {
            if seg.is_empty() && i == segs.len() - 1 {
                continue; // trailing newline of the block
            }
            self.begin_line(true);
            self.wrap_at(seg, Some(style), width);
        }
        self.pending = true;
    }

    fn rule(&mut self) {
        self.begin_line(true);
        // With `--width` set the rule spans the whole available line;
        // without it the rule stays a modest 40 columns. It deliberately
        // ignores `--table-width`/`--code-width`, which only size their own
        // blocks, so a table can end up wider or narrower than the rules.
        let n = self
            .widths
            .all
            .map_or(40, |w| w.saturating_sub(self.gutter_width()));
        if n > 0 {
            let _ = self.write_run(&"─".repeat(n), Some(plain_style(C_BORDER)));
        }
        self.pending = true;
    }

    /// Draw one table row (possibly multi-line cells) with padded columns.
    fn table_row(
        &mut self,
        cells: &[Vec<(String, Vec<u8>)>],
        widths: &[usize],
        aligns: &[Alignment],
    ) {
        let max_lines = cells.iter().map(|l| l.len()).max().unwrap_or(1);
        let empty: Vec<u8> = Vec::new();
        for line_idx in 0..max_lines {
            self.begin_line(true);
            self.write_run("│", Some(plain_style(C_BORDER)));
            for col in 0..widths.len() {
                let (content, styled) = match cells.get(col).and_then(|lines| lines.get(line_idx)) {
                    Some((p, s)) => (p.as_str(), s),
                    None => ("", &empty),
                };
                let align = aligns.get(col).copied().unwrap_or(Alignment::None);
                let (pre, post) = pad_info(content, widths[col], align);
                self.write_run(&pre, None);
                if !styled.is_empty() {
                    self.flush_spaces();
                    let _ = self.w.write_all(styled);
                    self.col += content.width();
                    self.line_has_content = true;
                    self.block_content = true;
                }
                self.write_run(&post, None);
                self.write_run("│", Some(plain_style(C_BORDER)));
            }
        }
    }

    /// Render a table whose events were batched between `Start(Table)` and
    /// `End(Table)`.
    fn table(&mut self, batch: &[Event<'_>], aligns: &[Alignment]) {
        // Collect rows of cells (inline events per cell).
        let mut rows: Vec<Vec<Vec<Event<'_>>>> = Vec::new();
        let mut row: Option<Vec<Vec<Event<'_>>>> = None;
        for ev in batch {
            match ev {
                Event::Start(Tag::TableHead) => row = Some(Vec::new()),
                Event::Start(Tag::TableRow) => row = Some(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    if let Some(r) = row.as_mut() {
                        r.push(Vec::new());
                    }
                }
                Event::End(TagEnd::TableRow) => {
                    if let Some(r) = std::mem::take(&mut row) {
                        rows.push(r);
                    }
                }
                Event::End(TagEnd::TableHead) => {
                    if let Some(r) = std::mem::take(&mut row) {
                        rows.push(r);
                    }
                }
                Event::End(TagEnd::Table) => break,
                other => {
                    if let Some(r) = &mut row {
                        if let Some(c) = r.last_mut() {
                            c.push(other.clone());
                        }
                    }
                }
            }
        }
        if rows.is_empty() {
            return;
        }

        // Render each cell twice: plain (to measure) and styled (to display),
        // as parallel (plain, styled) line pairs.
        let mut cells: Vec<Vec<Vec<(String, Vec<u8>)>>> = Vec::with_capacity(rows.len());
        for (r, row) in rows.iter().enumerate() {
            let base = if r == 0 {
                Style {
                    bold: true,
                    fg: Some(C_HEADER),
                    ..Default::default()
                }
            } else {
                Style::default()
            };
            let mut cc = Vec::with_capacity(row.len());
            for cell in row {
                let mut plain_buf: Vec<u8> = Vec::new();
                {
                    let mut em = Emitter::new(&mut plain_buf, false, base, Widths::default());
                    for ev in cell {
                        handle_event(&mut em, ev);
                    }
                }
                let mut styled_buf: Vec<u8> = Vec::new();
                {
                    let mut em = Emitter::new(&mut styled_buf, self.color, base, Widths::default());
                    for ev in cell {
                        handle_event(&mut em, ev);
                    }
                }
                let plain = String::from_utf8_lossy(&plain_buf).to_string();
                let styled = styled_buf;
                let mut lines: Vec<(String, Vec<u8>)> = Vec::new();
                for (pl, sl) in plain.split('\n').zip(styled.split(|&b| b == b'\n')) {
                    lines.push((pl.to_string(), sl.to_vec()));
                }
                cc.push(lines);
            }
            cells.push(cc);
        }

        // Column widths from the plain rendering.
        let mut widths = vec![0usize; rows[0].len()];
        for pc in &cells {
            for (c, lines) in pc.iter().enumerate() {
                if c >= widths.len() {
                    continue; // ragged row; ignore the extra cell
                }
                for (line, _) in lines {
                    widths[c] = widths[c].max(line.width());
                }
            }
        }

        // If a table width (or, failing that, the global width) is set,
        // shrink columns so the whole table fits in it
        // (overhead: one border per column plus two cell-edge paddings, and a
        // leading border; gutter prefixes count against the budget too).
        // Columns with content never shrink below 1: a 0-wide column would
        // leave its content unwrapped and the row borders would no longer
        // meet the separator's.
        if let Some(w) = self.widths.table() {
            let gutter = self.gutter_width();
            let overhead = 3 * widths.len() + 1;
            let budget = w.saturating_sub(gutter).saturating_sub(overhead) as i64;
            let mut sum: i64 = widths.iter().map(|&x| x as i64).sum();
            while sum > budget {
                let Some(mi) = widths
                    .iter()
                    .enumerate()
                    .filter(|&(_, &x)| x > 1)
                    .max_by_key(|&(_, x)| x)
                    .map(|(i, _)| i)
                else {
                    break;
                };
                widths[mi] -= 1;
                sum -= 1;
            }
        }

        // Wrap cell lines that are wider than their column.
        for row in cells.iter_mut() {
            for (c, lines) in row.iter_mut().enumerate() {
                let cw = *widths.get(c).unwrap_or(&0);
                let mut wrapped: Vec<(String, Vec<u8>)> = Vec::with_capacity(lines.len());
                for (plain_line, styled_line) in std::mem::take(lines) {
                    if plain_line.width() > cw {
                        wrapped.extend(wrap_pair(&plain_line, &styled_line, cw));
                    } else {
                        wrapped.push((plain_line, styled_line));
                    }
                }
                *lines = wrapped;
            }
        }

        for r in 0..rows.len() {
            self.table_row(&cells[r], &widths, aligns);
            if r == 0 {
                // Header separator.
                self.begin_line(true);
                self.write_run("├", Some(plain_style(C_BORDER)));
                for (i, w) in widths.iter().enumerate() {
                    self.write_run(&"─".repeat(w + 2), Some(plain_style(C_BORDER)));
                    if i + 1 == widths.len() {
                        self.write_run("┤", Some(plain_style(C_BORDER)));
                    } else {
                        self.write_run("┼", Some(plain_style(C_BORDER)));
                    }
                }
            }
        }
        self.pending = true;
        self.top_gap = true;
    }
}

fn pad_info(text: &str, width: usize, align: Alignment) -> (String, String) {
    let w = text.width();
    let pad = width.saturating_sub(w);
    let (l, r) = match align {
        Alignment::None | Alignment::Left => (0usize, pad),
        Alignment::Right => (pad, 0),
        Alignment::Center => (pad / 2, pad - pad / 2),
    };
    (" ".repeat(l + 1), " ".repeat(r + 1)) // 1 = cell edge padding
}

/// Total display width of a char slice.
fn width_of(pc: &[char]) -> usize {
    pc.iter().map(|c| c.width().unwrap_or(0)).sum()
}

/// Greedily word-wrap `pc` into lines of at most `first` columns for the
/// current line and `rest` columns for continuation lines. Returns exclusive
/// `(start, end)` char-index pairs; tokens wider than the capacity are
/// hard-broken.
fn wrap_char_ranges(pc: &[char], first: usize, rest: usize) -> Vec<(usize, usize)> {
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    let mut col = 0usize;
    let mut cap = first;
    let mut has = false;
    let mut i = 0usize;
    while i < pc.len() {
        let ws = pc[i].is_whitespace();
        let mut j = i;
        while j < pc.len() && pc[j].is_whitespace() == ws {
            j += 1;
        }
        if ws {
            if has {
                col += width_of(&pc[i..j]);
            }
            i = j;
            continue;
        }
        let w = width_of(&pc[i..j]);
        if has && col + w > cap {
            let mut e = i;
            while e > start && pc[e - 1].is_whitespace() {
                e -= 1;
            }
            lines.push((start, e));
            start = i;
            col = 0;
            cap = rest;
        }
        if !has && w > cap && w <= rest {
            // First token misses the current line's remainder but fits on
            // a fresh line: close the line and start there.
            let mut e = i;
            while e > start && pc[e - 1].is_whitespace() {
                e -= 1;
            }
            if e > start {
                lines.push((start, e));
            }
            start = i;
            col = 0;
            cap = rest;
        }
        if w > cap {
            // Hard-break the token across lines.
            let mut pos = i;
            let mut c = 0usize;
            while pos < j {
                let cw = pc[pos].width().unwrap_or(0);
                if c + cw > cap {
                    if pos > start {
                        lines.push((start, pos));
                    }
                    start = pos;
                    c = 0;
                    cap = rest;
                }
                if c + cw <= cap {
                    c += cw;
                    pos += 1;
                } else {
                    // Single char wider than a whole line: overflow it.
                    lines.push((start, pos + 1));
                    start = pos + 1;
                    c = 0;
                    cap = rest;
                    pos += 1;
                }
            }
            col = c;
        } else {
            col += w;
        }
        has = true;
        i = j;
    }
    if has {
        let mut e = pc.len();
        while e > start && pc[e - 1].is_whitespace() {
            e -= 1;
        }
        if e > start {
            lines.push((start, e));
        }
    }
    lines
}

/// Split a styled (ANSI) line and its plain counterpart into chunks of at
/// most `width` display columns, keeping each styled chunk aligned with its
/// plain chunk. A zero width still wraps (one character per line) so the
/// caller's borders never detach from an unwrapped line.
fn wrap_pair(plain: &str, styled: &[u8], width: usize) -> Vec<(String, Vec<u8>)> {
    if plain.is_empty() || plain.width() <= width {
        return vec![(plain.to_string(), styled.to_vec())];
    }
    // For each visible char record: the SGR open that styles it (if any),
    // where the char's bytes start, and the end of the bytes that draw it
    // (char + an immediately following reset, when present).
    let mut sgr: Vec<Option<(usize, usize)>> = Vec::new();
    let mut start_off: Vec<usize> = Vec::new();
    let mut close: Vec<usize> = Vec::new();
    let mut char_end: Vec<usize> = Vec::new();
    let mut i = 0usize;
    let mut cur: Option<(usize, usize)> = None;
    while i < styled.len() {
        if styled[i] == 0x1b && i + 2 < styled.len() && styled[i + 1] == b'[' {
            let mut j = i + 2;
            while j < styled.len() && !(styled[j] >= 0x40 && styled[j] <= 0x7e) {
                j += 1;
            }
            let seq_end = if j < styled.len() { j + 1 } else { j };
            let params = &styled[i + 2..j.min(styled.len())];
            // Params "0" (or empty) is a reset; anything else opens a style.
            if params.is_empty() || params == b"0" {
                if let Some(last) = close.len().checked_sub(1) {
                    close[last] = seq_end;
                }
                cur = None;
            } else {
                cur = Some((i, seq_end));
            }
            i = seq_end;
            continue;
        }
        let rest = std::str::from_utf8(&styled[i..]).unwrap_or("");
        let Some(ch) = rest.chars().next() else {
            break;
        };
        let end = i + ch.len_utf8();
        sgr.push(cur);
        start_off.push(i);
        close.push(end);
        char_end.push(end);
        i = end;
    }
    let pc: Vec<char> = plain.chars().collect();
    if pc.len() != sgr.len() {
        // Styled and plain renderings disagree; do not wrap.
        return vec![(plain.to_string(), styled.to_vec())];
    }
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for (a, b) in wrap_char_ranges(&pc, width, width) {
        let mut chunk: Vec<u8> = Vec::new();
        let mut opened = false;
        // A chunk starting mid-run re-emits its first char's SGR open.
        if let Some((gs, ge)) = sgr[a] {
            chunk.extend_from_slice(&styled[gs..ge]);
            opened = true;
        }
        for c in a + 1..b {
            opened |= sgr[c].is_some();
        }
        chunk.extend_from_slice(&styled[start_off[a]..close[b - 1]]);
        // Close any style still open at the chunk's end so the chunk is
        // self-contained.
        if opened && close[b - 1] == char_end[b - 1] {
            chunk.extend_from_slice(RESET.as_bytes());
        }
        let plain_chunk: String = pc[a..b].iter().collect();
        out.push((plain_chunk, chunk));
    }
    out
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

fn handle_event<W: Write>(em: &mut Emitter<W>, ev: &Event<'_>) {
    match ev {
        Event::Start(tag) => {
            if em.depth == 0 {
                em.block_content = false;
            }
            em.depth += 1;
            match tag {
                Tag::Paragraph => {
                    em.pending = !em.after_label;
                    em.after_label = false;
                }
                Tag::Heading { level, .. } => {
                    em.push_style();
                    em.style = heading_style(*level);
                    em.begin_line(true);
                    em.pending = false;
                    let markers = "#".repeat(*level as usize);
                    let _ = em.write_run(&format!("{markers} "), Some(plain_style(C_BORDER)));
                }
                Tag::BlockQuote(_) => {
                    em.gutters.push(Gutter {
                        kind: GutterKind::Quote,
                        first: "> ".to_string(),
                        first_style: Some(C_BORDER),
                        cont: "  ".to_string(),
                        started: false,
                    });
                    em.pending = true;
                }
                Tag::CodeBlock(kind) => {
                    em.gutters.push(Gutter {
                        kind: GutterKind::Code,
                        first: "│ ".to_string(),
                        first_style: Some(C_BORDER),
                        cont: "│ ".to_string(),
                        started: false,
                    });
                    em.code_lang = match kind {
                        CodeBlockKind::Fenced(s) if !s.is_empty() => Some(s.to_string()),
                        _ => None,
                    };
                    em.pending = true;
                }
                Tag::HtmlBlock => {
                    em.pending = true;
                }
                Tag::List(start) => {
                    em.ordered.push(start.map(|s| (s, s.to_string().len())));
                }
                Tag::Item => {
                    let (first, cont) = match em.ordered.last_mut().and_then(|e| e.as_mut()) {
                        Some((n, digits)) => {
                            let d = *digits;
                            let f = format!("{n}. ");
                            *n += 1;
                            (f, " ".repeat(d + 2))
                        }
                        None => ("• ".to_string(), "  ".to_string()),
                    };
                    em.gutters.push(Gutter {
                        kind: GutterKind::Item,
                        first,
                        first_style: Some(C_BULLET),
                        cont,
                        started: false,
                    });
                    em.pending = true;
                }
                Tag::FootnoteDefinition(label) => {
                    if em.pending {
                        em.begin_line(true);
                        em.pending = false;
                    }
                    let _ = em.wrap(&format!("[^{label}]: "), Some(plain_style(C_DIM)));
                    em.after_label = true;
                }
                Tag::MetadataBlock(_) => {
                    em.pending = true;
                }
                Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
                Tag::Table(_) => {} // batched by the caller
                Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
                // ---- inline spans ----
                Tag::Strong => {
                    em.push_style();
                    em.style.bold = true;
                    if em.style.fg.is_none() {
                        em.style.fg = Some(C_BOLD);
                    }
                }
                Tag::Emphasis => {
                    em.push_style();
                    em.style.italic = true;
                    if em.style.fg.is_none() {
                        em.style.fg = Some(C_ITALIC);
                    }
                }
                Tag::Strikethrough => {
                    em.push_style();
                    em.style.strike = true;
                    em.style.dim = true;
                }
                Tag::Superscript | Tag::Subscript => {
                    em.push_style();
                }
                Tag::Link {
                    link_type,
                    dest_url,
                    ..
                } => {
                    em.push_style();
                    em.style.underline = true;
                    if em.style.fg.is_none() {
                        em.style.fg = Some(C_LINK);
                    }
                    em.link_url = dest_url.to_string();
                    em.link_brackets = !matches!(link_type, LinkType::Autolink | LinkType::Email);
                    if em.link_brackets {
                        if em.pending {
                            em.begin_line(true);
                            em.pending = false;
                        }
                        let _ = em.write_run("[", Some(plain_style(C_DIM)));
                    }
                }
                Tag::Image { dest_url, .. } => {
                    em.push_style();
                    em.link_url = dest_url.to_string();
                    em.link_brackets = true;
                    if em.pending {
                        em.begin_line(true);
                        em.pending = false;
                    }
                    let _ = em.write_run("![", Some(plain_style(C_DIM)));
                }
            }
        }
        Event::End(tag_end) => {
            em.depth = em.depth.saturating_sub(1);
            if em.depth == 0 && em.block_content {
                em.top_gap = true;
            }
            match tag_end {
                TagEnd::Paragraph | TagEnd::HtmlBlock => {
                    em.pending = true;
                }
                TagEnd::CodeBlock => {
                    em.gutters.pop();
                    em.pending = true;
                }
                TagEnd::Heading(_) => {
                    em.pop_style();
                    em.pending = true;
                }
                TagEnd::BlockQuote(_) => {
                    em.gutters.pop();
                    em.pending = true;
                }
                TagEnd::List(_) => {
                    em.ordered.pop();
                }
                TagEnd::Item => {
                    em.gutters.pop();
                    em.pending = true;
                }
                TagEnd::FootnoteDefinition | TagEnd::MetadataBlock(_) => {
                    em.pending = true;
                }
                TagEnd::DefinitionList
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition => {}
                TagEnd::Strong
                | TagEnd::Emphasis
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript => {
                    em.pop_style();
                }
                TagEnd::Link | TagEnd::Image => {
                    let url = std::mem::take(&mut em.link_url);
                    if em.link_brackets {
                        let _ = em.write_run("]", Some(plain_style(C_DIM)));
                        if !url.is_empty() {
                            let _ = em.write_run("(", Some(plain_style(C_DIM)));
                            let _ = em.wrap(&url, Some(plain_style(C_DIM)));
                            let _ = em.write_run(")", Some(plain_style(C_BORDER)));
                        }
                    }
                    em.link_brackets = false;
                    em.pop_style();
                }
                TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            }
        }
        Event::Text(t) => {
            if em.in_code_block() {
                em.emit_code(t.as_ref());
            } else {
                let _ = em.emit_text(t, None);
            }
        }
        Event::Code(t) => {
            if em.in_code_block() {
                em.emit_code(t.as_ref());
            } else {
                let _ = em.emit_text(t.as_ref(), Some(plain_style(C_CODE)));
            }
        }
        Event::InlineMath(t) | Event::DisplayMath(t) => {
            let s = Style {
                italic: true,
                fg: Some(C_MATH),
                ..Default::default()
            };
            if matches!(ev, Event::DisplayMath(_)) {
                em.begin_line(true);
            }
            let _ = em.emit_text(&format!("${t}$"), Some(s));
        }
        Event::Html(t) => {
            let _ = em.emit_text(t.as_ref(), Some(plain_style(C_DIM)));
            em.pending = true;
        }
        Event::InlineHtml(t) => {
            if em.pending {
                em.begin_line(true);
                em.pending = false;
            }
            let _ = em.wrap(t.as_ref(), Some(plain_style(C_DIM)));
        }
        Event::FootnoteReference(label) => {
            let _ = em.emit_text(&format!("[^{label}]"), Some(plain_style(C_ITALIC)));
        }
        Event::TaskListMarker(checked) => {
            let _ = em.emit_text(
                if *checked { "[x] " } else { "[ ] " },
                Some(plain_style(if *checked { C_BULLET } else { C_DIM })),
            );
        }
        Event::SoftBreak | Event::HardBreak => {
            em.begin_line(true);
        }
        Event::Rule => em.rule(),
    }
}

// ---------------------------------------------------------------------------
// Top-level rendering
// ---------------------------------------------------------------------------

/// Output width limits, in display columns; `None` = no limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Widths {
    /// Everything (`--width`).
    all: Option<usize>,
    /// Tables (`--table-width`): `None` falls back to `all`; `Some(None)`
    /// (`--no-table-width`) means unlimited regardless of `all`.
    table: Option<Option<usize>>,
    /// Code blocks (`--code-width`), with the same fallback as `table`.
    code: Option<Option<usize>>,
}

impl Widths {
    /// Effective table budget: `--table-width`, else `--width`.
    fn table(&self) -> Option<usize> {
        self.table.unwrap_or(self.all)
    }

    /// Effective code-block wrap width: `--code-width`, else `--width`.
    fn code(&self) -> Option<usize> {
        self.code.unwrap_or(self.all)
    }

    /// Set the limit for a width option (by canonical name); `None` is the
    /// `--no-…` form.
    fn set(&mut self, flag: &str, limit: Option<usize>) {
        match flag {
            "--table-width" => self.table = Some(limit),
            "--code-width" => self.code = Some(limit),
            _ => self.all = limit,
        }
    }
}

fn render(md: &str, color: bool, widths: Widths) -> Vec<u8> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let mut iter = Parser::new_ext(md, options);
    let mut buf: Vec<u8> = Vec::new();
    let mut em = Emitter::new(&mut buf, color, Style::default(), widths);

    while let Some(ev) = iter.next() {
        if let Event::Start(Tag::Table(aligns)) = &ev {
            let aligns = aligns.clone();
            let mut batch = vec![ev.clone()];
            let mut depth: usize = 1;
            let mut done = false;
            while !done {
                let inner = match iter.next() {
                    Some(e) => e,
                    None => break,
                };
                match &inner {
                    Event::Start(_) => depth += 1,
                    Event::End(e) if *e == TagEnd::Table && depth == 1 => done = true,
                    Event::End(_) => depth = depth.saturating_sub(1),
                    _ => {}
                }
                batch.push(inner);
            }
            em.table(&batch, &aligns);
            continue;
        }
        handle_event(&mut em, &ev);
    }

    if em.line_has_content {
        let _ = em.w.write_all(b"\n");
    }
    buf
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

const HELP: &str = "\
mdhl — highlight Markdown as ANSI-colored text

Usage: mdhl [OPTIONS] [FILE]

Reads Markdown from FILE, or from stdin when omitted, and writes a
highlighted rendering to stdout.

Options:
  --color[=WHEN]      Force colors. WHEN: always (default for this flag),
                      never, auto (TTY + no NO_COLOR)
  --no-color          Disable ANSI colors (same as --color=never)
  -w, --width N       Wrap output at N display columns; no wrapping by default
  --table-width N     Fit tables in N display columns, overriding --width
                      for tables only (wider or narrower)
  --code-width N      Wrap code blocks at N display columns, overriding
                      --width for code blocks only (wider or narrower)
  --no-width, --no-table-width, --no-code-width
                      Remove that width limit (e.g. one set in the config)
  --no-config         Ignore the config file
  -h, --help          Show this help
  -V, --version       Show version

Config: default options are read from the first of $MDHL_CONFIG,
$XDG_CONFIG_HOME/mdhl/config (~/.config/mdhl/config), /etc/mdhl/config
that exists. It holds options as on the command line, whitespace- or
line-separated; `#` starts a comment line. Command-line options win.
";

/// System-wide config, used when no user config exists.
const SYSTEM_CONFIG: &str = "/etc/mdhl/config";

fn parse_width(flag: &str, v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "invalid {flag} value: {v} (expected a positive integer)"
        )),
    }
}

/// Recognize a width option: returns its canonical name and, for the
/// `--flag=N` / `-wN` forms, the attached value.
fn width_flag(a: &str) -> Option<(&'static str, Option<&str>)> {
    for flag in ["--width", "--table-width", "--code-width"] {
        if a == flag {
            return Some((flag, None));
        }
        if let Some(v) = a.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
            return Some((flag, Some(v)));
        }
    }
    match a.strip_prefix("-w") {
        Some("") => Some(("--width", None)),
        Some(v) => Some(("--width", Some(v))),
        None => None,
    }
}

/// Recognize `--no-width` & co.: returns the width option it clears.
fn no_width_flag(a: &str) -> Option<&'static str> {
    match a {
        "--no-width" => Some("--width"),
        "--no-table-width" => Some("--table-width"),
        "--no-code-width" => Some("--code-width"),
        _ => None,
    }
}

/// Settings gathered from the config file and the command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    /// `None` = auto (TTY and no `NO_COLOR`).
    color: Option<bool>,
    widths: Widths,
    file: Option<String>,
    no_config: bool,
}

/// What `main` should do once arguments are parsed.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Run,
    Help,
    Version,
}

#[derive(Debug)]
struct ArgError {
    msg: String,
    /// Follow the message with the help text (unknown options).
    show_help: bool,
}

impl From<String> for ArgError {
    fn from(msg: String) -> Self {
        ArgError {
            msg,
            show_help: false,
        }
    }
}

/// Apply `args` to `opts`, later options overriding earlier ones.
/// `cli` is false for config-file arguments, which may only set option
/// defaults: no FILE, `--help`, `--version`, or `--no-config`.
fn parse_args(args: &[String], opts: &mut Opts, cli: bool) -> Result<Flow, ArgError> {
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" | "-V" | "--version" | "--no-config" if !cli => {
                return Err(format!("{a} is not allowed in a config file").into());
            }
            "-h" | "--help" => return Ok(Flow::Help),
            "-V" | "--version" => return Ok(Flow::Version),
            "--no-config" => opts.no_config = true,
            "--no-color" => opts.color = Some(false),
            "--color" => opts.color = Some(true),
            s if s.starts_with("--color=") => {
                opts.color = match &s["--color=".len()..] {
                    "always" => Some(true),
                    "never" => Some(false),
                    "auto" => None,
                    other => {
                        return Err(format!(
                            "invalid --color value: {other} (expected always, never, or auto)"
                        )
                        .into());
                    }
                };
            }
            _ if let Some(flag) = no_width_flag(a) => opts.widths.set(flag, None),
            _ if let Some((flag, inline)) = width_flag(a) => {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.as_str(),
                            None => return Err(format!("{flag} requires a value").into()),
                        }
                    }
                };
                opts.widths.set(flag, Some(parse_width(flag, v)?));
            }
            s if s.starts_with('-') && s != "-" => {
                return Err(ArgError {
                    msg: format!("unknown option: {a}"),
                    show_help: cli,
                });
            }
            _ if !cli => {
                return Err(format!(
                    "unexpected argument: {a} (FILE can't be set in a config file)"
                )
                .into());
            }
            _ => {
                if opts.file.is_some() {
                    return Err(format!("unexpected argument: {a}").into());
                }
                opts.file = Some(a.to_string());
            }
        }
        i += 1;
    }
    Ok(Flow::Run)
}

/// Config files to try, in order; the first that exists is used. When
/// `required`, the (single) path was named explicitly and must exist.
struct ConfigSearch {
    paths: Vec<PathBuf>,
    required: bool,
}

fn config_search(var: impl Fn(&str) -> Option<OsString>) -> ConfigSearch {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    if let Some(p) = set("MDHL_CONFIG") {
        return ConfigSearch {
            paths: vec![PathBuf::from(p)],
            required: true,
        };
    }
    let mut paths = Vec::new();
    // Per the XDG spec, a relative XDG_CONFIG_HOME is invalid and ignored.
    let user_dir = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| set("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(dir) = user_dir {
        paths.push(dir.join("mdhl").join("config"));
    }
    paths.push(PathBuf::from(SYSTEM_CONFIG));
    ConfigSearch {
        paths,
        required: false,
    }
}

/// Split config text into arguments: whitespace-separated, with blank
/// lines and lines starting with `#` ignored.
fn config_args(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(String::from)
        .collect()
}

/// Read the first existing config file: its path and arguments, or
/// `None` if there is none.
fn load_config(search: &ConfigSearch) -> Result<Option<(PathBuf, Vec<String>)>, String> {
    for path in &search.paths {
        match std::fs::read_to_string(path) {
            Ok(text) => return Ok(Some((path.clone(), config_args(&text)))),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !search.required => continue,
            Err(e) => return Err(format!("cannot read config {}: {e}", path.display())),
        }
    }
    Ok(None)
}

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();

    // The command line is validated on its own first, so `--help` and
    // usage errors never depend on the config file.
    let mut opts = Opts::default();
    match parse_args(&args, &mut opts, true) {
        Ok(Flow::Run) => {}
        Ok(Flow::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Flow::Version) => {
            println!("mdhl {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("mdhl: {}", e.msg);
            if e.show_help {
                eprint!("{HELP}");
            }
            return ExitCode::from(2);
        }
    }

    if !opts.no_config {
        match load_config(&config_search(|k| env::var_os(k))) {
            Ok(None) => {}
            Ok(Some((path, config))) => {
                // Config first, then the command line on top of it.
                let mut merged = Opts::default();
                if let Err(e) = parse_args(&config, &mut merged, false) {
                    eprintln!("mdhl: {}: {}", path.display(), e.msg);
                    return ExitCode::from(2);
                }
                parse_args(&args, &mut merged, true).expect("command line already validated");
                opts = merged;
            }
            Err(msg) => {
                eprintln!("mdhl: {msg}");
                return ExitCode::from(2);
            }
        }
    }

    let color = opts
        .color
        .unwrap_or_else(|| io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none());

    let input = match &opts.file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mdhl: cannot read {path}: {e}");
                return ExitCode::from(2);
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut s) {
                eprintln!("mdhl: cannot read stdin: {e}");
                return ExitCode::from(2);
            }
            s
        }
    };

    let out = render(&input, color, opts.widths);
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if let Err(e) = handle.write_all(&out) {
        if e.kind() == io::ErrorKind::BrokenPipe {
            // Reader (e.g. `| head`) went away; exit like `cat` would.
            return ExitCode::from(141);
        }
        eprintln!("mdhl: write error: {e}");
        return ExitCode::FAILURE;
    }
    let _ = handle.flush();
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Tests: plain (color-off) rendering is the observable contract.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{Widths, render};
    use unicode_width::UnicodeWidthStr;

    fn plain(md: &str) -> String {
        String::from_utf8(render(md, false, Widths::default())).expect("utf8 output")
    }

    fn plain_w(md: &str, width: usize) -> String {
        String::from_utf8(render(md, false, all(width))).expect("utf8 output")
    }

    /// Remove SGR escape sequences so colored output can be compared as text.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut skip = 0usize;
        for c in s.chars() {
            if skip > 0 {
                if c == 'm' {
                    skip = 0;
                }
            } else if c == '\u{1b}' {
                skip = 1;
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn blocks_are_separated_by_blank_lines() {
        assert_eq!(plain("a\n\nb\n"), "a\n\nb\n");
        assert_eq!(plain("# T\n\npara"), "# T\n\npara\n");
    }

    #[test]
    fn inline_styles_render_plainly() {
        assert_eq!(
            plain("para *it* **bo** `code` [l](u)"),
            "para it bo code [l](u)\n"
        );
    }

    #[test]
    fn quote_lines_and_nesting() {
        assert_eq!(plain("> quote\n> line"), "> quote\n> line\n");
        assert_eq!(
            plain("> hello\n>\n> ```\n> code\n> ```\n>\n> - x"),
            "> hello\n> │ code\n> • x\n"
        );
    }

    #[test]
    fn list_gutters() {
        assert_eq!(
            plain("- a\n  - b\n\n1. one\n2. two"),
            "• a\n  • b\n\n1. one\n2. two\n"
        );
    }

    #[test]
    fn code_blocks_get_border() {
        assert_eq!(
            plain("```rust\nfn main() {}\n```"),
            "rust\n│ fn main() {}\n"
        );
        assert_eq!(plain("    indented code"), "│ indented code\n");
    }

    #[test]
    fn table_pads_columns() {
        assert_eq!(
            plain("| a | bb |\n|---|----|\n| 1 | 2  |"),
            "│ a │ bb │\n├───┼────┤\n│ 1 │ 2  │\n"
        );
    }

    #[test]
    fn color_toggle() {
        let off = render("x **y**", false, Widths::default());
        assert!(!off.contains(&0x1b));
        let on = render("x **y**", true, Widths::default());
        let open = b"\x1b[1;38;5;220m";
        assert!(on.windows(open.len()).any(|w| w == open));
        assert!(on.ends_with(b"\n".as_slice()));
    }

    #[test]
    fn width_wraps_paragraphs() {
        assert_eq!(
            plain_w("hello world this is a test", 10),
            "hello\nworld this\nis a test\n"
        );
        assert_eq!(plain_w("hello world", 40), "hello world\n");
    }

    #[test]
    fn width_moves_whole_tokens() {
        // A token that misses the line's remainder but fits on a fresh line
        // moves whole instead of being hard-broken.
        assert_eq!(
            plain_w("x y z [a](http://x.y)", 12),
            "x y z [a](\nhttp://x.y)\n"
        );
    }

    #[test]
    fn width_wraps_quotes_and_code() {
        assert_eq!(plain_w("> hello world foo", 9), "> hello\n  world\n  foo\n");
        assert_eq!(plain_w("```\nabcdefghij\n```", 6), "│ abcd\n│ efgh\n│ ij\n");
    }

    #[test]
    fn width_shrinks_tables() {
        let md = "| aaaa | bbbb |\n|------|------|\n| 1111 | 2222 |";
        let out = plain_w(md, 12);
        for line in out.lines() {
            assert!(line.width() <= 12, "line too wide: {line:?}");
        }
        assert!(out.starts_with("│ aaa │ bb │\n│ a   │ bb │\n├─────┼────┤\n"));
    }

    #[test]
    fn width_keeps_colored_tables_aligned() {
        let md = "| aaaa | bbbb |\n|------|------|\n| 1111 | 2222 |";
        let plain = plain_w(md, 12);
        let on = render(md, true, all(12));
        let colored = String::from_utf8(on).expect("utf8 output");
        assert!(colored.contains('\u{1b}'));
        assert_eq!(strip_ansi(&colored), plain);
        for line in colored.lines() {
            let w = strip_ansi(line).width();
            assert!(w <= 12, "line too wide: {line:?}");
        }
    }

    #[test]
    fn width_table_borders_stay_aligned() {
        // When the budget cannot keep every column at width 1, shrinking
        // stops at 1 per column instead of collapsing a column to 0: a
        // 0-wide column would leave its content unwrapped and the row
        // borders would no longer meet the separator's.
        let md = "| aaaa | bbbb | cccc |\n|------|------|------|\n| 1111 | 2222 | 3333 |";
        for w in [8usize, 12, 14, 20] {
            let out = plain_w(md, w);
            // Width-1 columns route every cell through wrap_pair's per-char
            // SGR chunking when color is on; the stripped colored text must
            // be identical to plain and keep the same uniform line widths.
            let colored = String::from_utf8(render(md, true, all(w))).expect("utf8 output");
            assert_eq!(
                strip_ansi(&colored),
                out,
                "w={w}: colored differs from plain"
            );
            let is_table = |l: &str| l.contains('│') || l.contains('├');
            let table: Vec<&str> = out.lines().filter(|l| is_table(l)).collect();
            let ws: Vec<usize> = table.iter().map(|l| l.width()).collect();
            assert_eq!(
                ws.iter().all(|&x| x == ws[0]),
                true,
                "w={w}: misaligned table lines {ws:?}\n{out}"
            );
            for line in colored.lines() {
                let s = strip_ansi(line);
                if is_table(&s) {
                    assert_eq!(s.width(), ws[0], "w={w}: colored line misaligned: {s:?}");
                }
            }
            // A 3-column table needs 4 columns per column plus one leading
            // border; below that it must overflow, but stay aligned.
            if w >= 13 {
                for line in &table {
                    assert!(line.width() <= w, "w={w}: line too wide: {line:?}");
                }
            }
        }
    }

    #[test]
    fn width_table_in_quote_fits_budget() {
        // Gutter prefixes count against the width budget.
        let md = "> | aaaa | bbbb |\n> |------|------|\n> | 1111 | 2222 |";
        let out = plain_w(md, 13);
        for line in out.lines() {
            assert!(line.width() <= 13, "line too wide: {line:?}");
        }
    }

    #[test]
    fn width_rules_span_full_line() {
        // With a width set the rule fills the available line; gutters count.
        assert_eq!(plain_w("---", 8), format!("{}\n", "─".repeat(8)));
        assert_eq!(plain_w("---", 120), format!("{}\n", "─".repeat(120)));
        assert_eq!(plain_w("> ---", 10), format!("> {}\n", "─".repeat(8)));
        // Without a width the rule stays a fixed 40 columns.
        assert_eq!(plain("---"), format!("{}\n", "─".repeat(40)));
    }

    #[test]
    fn width_parsing_rejects_bad_values() {
        use super::parse_width;
        for flag in ["--width", "--table-width", "--code-width"] {
            assert_eq!(parse_width(flag, "40"), Ok(40));
            for bad in ["0", "abc", "-3", ""] {
                let err = parse_width(flag, bad).unwrap_err();
                assert!(
                    err.contains(&format!("invalid {flag} value: {bad} ")),
                    "{flag} {bad:?}: {err}"
                );
            }
        }
    }

    #[test]
    fn width_flags_set_their_own_field() {
        // Each option (in every spelling) lands in its own Widths field.
        for (arg, value) in [
            ("--width", 1),
            ("-w", 1),
            ("--table-width", 2),
            ("--code-width", 3),
        ] {
            let (flag, _) = super::width_flag(arg).unwrap();
            let mut w = Widths::default();
            w.set(flag, Some(value));
            let expect = match value {
                1 => Widths {
                    all: Some(1),
                    ..Default::default()
                },
                2 => Widths {
                    table: Some(Some(2)),
                    ..Default::default()
                },
                _ => Widths {
                    code: Some(Some(3)),
                    ..Default::default()
                },
            };
            assert_eq!(w, expect, "{arg}");
        }
    }

    #[test]
    fn widths_fall_back_to_all() {
        let w = Widths {
            all: Some(50),
            table: None,
            code: Some(Some(70)),
        };
        assert_eq!(w.table(), Some(50));
        assert_eq!(w.code(), Some(70));
        assert_eq!(Widths::default().table(), None);
    }

    fn all(w: usize) -> Widths {
        Widths {
            all: Some(w),
            ..Default::default()
        }
    }

    fn plain_with(md: &str, widths: Widths) -> String {
        String::from_utf8(render(md, false, widths)).expect("utf8 output")
    }

    #[test]
    fn code_width_overrides_width_for_code_only() {
        let code = "```\nabcdefghij\n```";
        // Narrower than --width: code wraps at 6, as with --width 6.
        assert_eq!(
            plain_with(
                code,
                Widths {
                    all: Some(80),
                    code: Some(Some(6)),
                    ..Default::default()
                }
            ),
            plain_w(code, 6)
        );
        // Wider than --width: code lines are left whole.
        assert_eq!(
            plain_with(
                code,
                Widths {
                    all: Some(6),
                    code: Some(Some(80)),
                    ..Default::default()
                }
            ),
            "│ abcdefghij\n"
        );
        // Prose still follows --width.
        assert_eq!(
            plain_with(
                "aaa bbb ccc",
                Widths {
                    all: Some(7),
                    code: Some(Some(80)),
                    ..Default::default()
                }
            ),
            "aaa bbb\nccc\n"
        );
        // Without --width only code wraps.
        let md = "aaa bbb ccc ddd\n\n```\nabcdefghij\n```";
        assert_eq!(
            plain_with(
                md,
                Widths {
                    code: Some(Some(6)),
                    ..Default::default()
                }
            ),
            "aaa bbb ccc ddd\n\n│ abcd\n│ efgh\n│ ij\n"
        );
    }

    #[test]
    fn long_code_caption_wraps_without_code_border() {
        // Wrapped caption lines keep the caption's prefix (no `│ `) and stay
        // within the code width, at top level and inside a blockquote.
        let code = |md| {
            plain_with(
                md,
                Widths {
                    code: Some(Some(8)),
                    ..Default::default()
                },
            )
        };
        assert_eq!(
            code("```verylonglanguagename\nabc\n```\n"),
            "verylong\nlanguage\nname\n│ abc\n"
        );
        let quoted = plain_with(
            "> ```verylonglanguagename\n> abc\n> ```\n",
            Widths {
                code: Some(Some(10)),
                ..Default::default()
            },
        );
        assert_eq!(quoted, "> verylong\n  language\n  name\n> │ abc\n");
    }

    #[test]
    fn width_flag_forms() {
        use super::width_flag;
        assert_eq!(width_flag("--width"), Some(("--width", None)));
        assert_eq!(width_flag("--width=5"), Some(("--width", Some("5"))));
        assert_eq!(width_flag("-w"), Some(("--width", None)));
        assert_eq!(width_flag("-w5"), Some(("--width", Some("5"))));
        assert_eq!(
            width_flag("--table-width=9"),
            Some(("--table-width", Some("9")))
        );
        assert_eq!(width_flag("--code-width"), Some(("--code-width", None)));
        assert_eq!(width_flag("--widthx"), None);
        assert_eq!(width_flag("--color"), None);
    }

    #[test]
    fn table_width_overrides_width_for_tables_only() {
        let table = "| aaaa | bbbb |\n|------|------|\n| 1111 | 2222 |";
        // Narrower than --width: the table shrinks to 12, as with --width 12.
        assert_eq!(
            plain_with(
                table,
                Widths {
                    all: Some(80),
                    table: Some(Some(12)),
                    ..Default::default()
                }
            ),
            plain_w(table, 12)
        );
        // Wider than --width: the table keeps its natural width and its
        // borders are not wrapped by the global width.
        assert_eq!(
            plain_with(
                table,
                Widths {
                    all: Some(8),
                    table: Some(Some(80)),
                    ..Default::default()
                }
            ),
            plain(table)
        );
        // Paragraphs and rules still follow --width.
        assert_eq!(
            plain_with(
                "aaa bbb ccc",
                Widths {
                    all: Some(7),
                    table: Some(Some(80)),
                    ..Default::default()
                }
            ),
            "aaa bbb\nccc\n"
        );
        assert_eq!(
            plain_with(
                "---",
                Widths {
                    all: Some(8),
                    table: Some(Some(80)),
                    ..Default::default()
                }
            ),
            format!("{}\n", "─".repeat(8))
        );
    }

    #[test]
    fn table_width_without_width() {
        // Tables fit the budget while other text stays unwrapped.
        let md = "aaa bbb ccc ddd eee\n\n| aaaa | bbbb |\n|------|------|\n| 1111 | 2222 |";
        let out = plain_with(
            md,
            Widths {
                table: Some(Some(12)),
                ..Default::default()
            },
        );
        assert!(out.starts_with("aaa bbb ccc ddd eee\n"));
        for line in out.lines().skip(1) {
            assert!(line.width() <= 12, "line too wide: {line:?}");
        }
    }

    // ---- config file / command-line merging ----

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// Parse `config` then `cli` the way `main` does.
    fn merged(config: &str, cli: &str) -> super::Opts {
        use super::{Opts, config_args, parse_args};
        let mut o = Opts::default();
        parse_args(&config_args(config), &mut o, false).expect("config parses");
        parse_args(&args(cli), &mut o, true).expect("cli parses");
        o
    }

    #[test]
    fn config_args_split_lines_and_skip_comments() {
        let text =
            "# defaults\n--width 80\n\n  --table-width=120  \n   # indented comment\n--color\n";
        assert_eq!(
            super::config_args(text),
            args("--width 80 --table-width=120 --color")
        );
    }

    #[test]
    fn command_line_overrides_config() {
        let cfg = "-w 80\n--table-width 120\n--code-width 120\n--color";
        let o = merged(cfg, "");
        assert_eq!(o.color, Some(true));
        assert_eq!(o.widths.all, Some(80));
        assert_eq!(o.widths.table(), Some(120));
        assert_eq!(o.widths.code(), Some(120));

        let o = merged(cfg, "-w 60 --color=never doc.md");
        assert_eq!(o.color, Some(false));
        assert_eq!(o.widths.all, Some(60));
        assert_eq!(o.widths.table(), Some(120));
        assert_eq!(o.file.as_deref(), Some("doc.md"));

        // --color=auto resets a forced color back to auto-detection.
        assert_eq!(merged(cfg, "--color=auto").color, None);
    }

    #[test]
    fn no_width_flags_remove_limits() {
        let cfg = "-w 80 --table-width 120 --code-width 100";
        let o = merged(cfg, "--no-width");
        assert_eq!(o.widths.all, None);
        assert_eq!(o.widths.table(), Some(120));
        // --no-table-width means unlimited, not "follow --width".
        let o = merged(cfg, "--no-table-width --no-code-width");
        assert_eq!(o.widths.all, Some(80));
        assert_eq!(o.widths.table(), None);
        assert_eq!(o.widths.code(), None);
        // A later width option sets the limit again.
        assert_eq!(
            merged(cfg, "--no-code-width --code-width 50").widths.code(),
            Some(50)
        );
        // Without a config, --no-width keeps tables/code falling back.
        assert_eq!(merged("", "--no-width").widths, Widths::default());
    }

    #[test]
    fn config_rejects_cli_only_arguments() {
        use super::{Opts, parse_args};
        for bad in [
            "--help",
            "-V",
            "--no-config",
            "doc.md",
            "--bogus",
            "--width",
        ] {
            let err = parse_args(&args(bad), &mut Opts::default(), false).unwrap_err();
            assert!(!err.show_help, "{bad}: config errors don't print help");
            assert!(!err.msg.is_empty());
        }
        // The same help/version flags work on the command line.
        let mut o = Opts::default();
        assert_eq!(
            parse_args(&args("--help"), &mut o, true).unwrap(),
            super::Flow::Help
        );
        assert_eq!(
            parse_args(&args("--no-config"), &mut o, true).unwrap(),
            super::Flow::Run
        );
        assert!(o.no_config);
    }

    #[test]
    fn config_search_order() {
        use super::{SYSTEM_CONFIG, config_search};
        use std::ffi::OsString;
        use std::path::PathBuf;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let paths = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();

        let s = config_search(env(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")]));
        assert_eq!(s.paths, paths(&["/x/mdhl/config", SYSTEM_CONFIG]));
        assert!(!s.required);

        // Unset, empty, or relative XDG_CONFIG_HOME falls back to ~/.config.
        const FALLBACKS: [&[(&str, &str)]; 3] = [
            &[("HOME", "/h")],
            &[("XDG_CONFIG_HOME", ""), ("HOME", "/h")],
            &[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")],
        ];
        for pairs in FALLBACKS {
            assert_eq!(
                config_search(env(pairs)).paths,
                paths(&["/h/.config/mdhl/config", SYSTEM_CONFIG])
            );
        }

        // No HOME at all: only the system config.
        assert_eq!(config_search(env(&[])).paths, paths(&[SYSTEM_CONFIG]));

        // MDHL_CONFIG replaces the search and must exist.
        let s = config_search(env(&[("MDHL_CONFIG", "/c"), ("HOME", "/h")]));
        assert_eq!(s.paths, paths(&["/c"]));
        assert!(s.required);
    }
}
