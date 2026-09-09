//! mdhl — read Markdown and print an ANSI-color highlighted version to stdout.
//!
//! Input: a positional `FILE`, or stdin when omitted.
//!
//! Color policy: ANSI escapes are emitted when stdout is a TTY, unless
//! `NO_COLOR` is set or `--no-color` is given; `--color` forces them on.

//! Width: `--width N` wraps all output to at most N display columns.

use std::env;
use std::io::{self, IsTerminal, Read, Write};
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
    /// A footnote-definition paragraph continues on its label line.
    after_label: bool,
    /// Max display columns for wrapping; `None` = no wrapping.
    width: Option<usize>,
    /// Display column the next byte lands in (visible columns on the line).
    col: usize,
    /// Column at which the current line's content starts (after gutters).
    line_start_col: usize,
    /// Trailing spaces of the last run, held back until the line is known
    /// to continue; dropped when it breaks.
    pending_sp: usize,
}

impl<'w, W: Write> Emitter<'w, W> {
    fn new(w: &'w mut W, color: bool, base_style: Style, width: Option<usize>) -> Self {
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
            after_label: false,
            width,
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
        self.begin_line_impl(new_block, false);
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

    /// Write `text` as one run, wrapping at word boundaries when a width is
    /// set. A token longer than a full line is hard-broken characterwise.
    fn wrap(&mut self, text: &str, style: Option<Style>) {
        if text.is_empty() {
            return;
        }
        let width = match self.width {
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
    fn emit_code(&mut self, text: &str) {
        if let Some(lang) = self.code_lang.take() {
            if !lang.is_empty() {
                self.begin_caption();
                let _ = self.wrap(
                    &lang,
                    Some(Style {
                        italic: true,
                        fg: Some(C_ITALIC),
                        dim: true,
                        ..Default::default()
                    }),
                );
            }
        }
        let segs: Vec<&str> = text.split('\n').collect();
        let style = plain_style(C_CODEBLOCK);
        for (i, seg) in segs.iter().enumerate() {
            if seg.is_empty() && i == segs.len() - 1 {
                continue; // trailing newline of the block
            }
            self.begin_line(true);
            let _ = self.wrap(seg, Some(style));
        }
        self.pending = true;
    }

    fn rule(&mut self) {
        self.begin_line(true);
        // With a width set the rule spans the whole available line (so it
        // matches blocks like tables that fill it); without one it stays a
        // modest 40 columns.
        let n = self
            .width
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
                    let mut em = Emitter::new(&mut plain_buf, false, base, None);
                    for ev in cell {
                        handle_event(&mut em, ev);
                    }
                }
                let mut styled_buf: Vec<u8> = Vec::new();
                {
                    let mut em = Emitter::new(&mut styled_buf, self.color, base, None);
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

        // If a width is set, shrink columns so the whole table fits in it
        // (overhead: one border per column plus two cell-edge paddings, and a
        // leading border; gutter prefixes count against the budget too).
        // Columns with content never shrink below 1: a 0-wide column would
        // leave its content unwrapped and the row borders would no longer
        // meet the separator's.
        if let Some(w) = self.width {
            let gutter = self.gutter_width();
            let overhead = 3 * widths.len() + 1;
            let budget = w
                .saturating_sub(gutter)
                .saturating_sub(overhead) as i64;
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

fn render(md: &str, color: bool, width: Option<usize>) -> Vec<u8> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let mut iter = Parser::new_ext(md, options);
    let mut buf: Vec<u8> = Vec::new();
    let mut em = Emitter::new(&mut buf, color, Style::default(), width);

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
  --color[=WHEN]   Force colors. WHEN: always (default for this flag),
                   never, auto (TTY + no NO_COLOR)
  --no-color       Disable ANSI colors (same as --color=never)
  -w, --width N    Wrap output at N display columns; no wrapping by default
  -h, --help       Show this help
  -V, --version    Show version
";

fn parse_width(v: &str) -> Option<usize> {
    match v.parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => {
            eprintln!("mdhl: invalid --width value: {v} (expected a positive integer)");
            None
        }
    }
}

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut color: Option<bool> = None;
    let mut width: Option<usize> = None;
    let mut file: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("mdhl {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--no-color" => color = Some(false),
            "--color" => color = Some(true),
            s if s.starts_with("--color=") => {
                color = match &s["--color=".len()..] {
                    "always" => Some(true),
                    "never" => Some(false),
                    "auto" => None,
                    other => {
                        eprintln!(
                            "mdhl: invalid --color value: {other} (expected always, never, or auto)"
                        );
                        return ExitCode::from(2);
                    }
                };
            }
            "--width" | "-w" => {
                i += 1;
                let v = match args.get(i) {
                    Some(v) => v,
                    None => {
                        eprintln!("mdhl: --width requires a value");
                        return ExitCode::from(2);
                    }
                };
                let Some(n) = parse_width(v) else {
                    return ExitCode::from(2);
                };
                width = Some(n);
            }
            s if s.starts_with("--width=") => {
                let Some(n) = parse_width(&s["--width=".len()..]) else {
                    return ExitCode::from(2);
                };
                width = Some(n);
            }
            s if s.len() > 2 && s.starts_with("-w") => {
                let Some(n) = parse_width(&a[2..]) else {
                    return ExitCode::from(2);
                };
                width = Some(n);
            }
            s if s.starts_with('-') && s != "-" => {
                eprintln!("mdhl: unknown option: {a}");
                eprint!("{HELP}");
                return ExitCode::from(2);
            }
            _ => {
                if file.is_some() {
                    eprintln!("mdhl: unexpected argument: {a}");
                    return ExitCode::from(2);
                }
                file = Some(a.to_string());
            }
        }
        i += 1;
    }

    let color =
        color.unwrap_or_else(|| io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none());

    let input = match &file {
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

    let out = render(&input, color, width);
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
    use super::render;
    use unicode_width::UnicodeWidthStr;

    fn plain(md: &str) -> String {
        String::from_utf8(render(md, false, None)).expect("utf8 output")
    }

    fn plain_w(md: &str, width: usize) -> String {
        String::from_utf8(render(md, false, Some(width))).expect("utf8 output")
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
        let off = render("x **y**", false, None);
        assert!(!off.contains(&0x1b));
        let on = render("x **y**", true, None);
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
        let on = render(md, true, Some(12));
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
            let colored = String::from_utf8(render(md, true, Some(w))).expect("utf8 output");
            assert_eq!(strip_ansi(&colored), out, "w={w}: colored differs from plain");
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
        assert_eq!(
            plain_w("> ---", 10),
            format!("> {}\n", "─".repeat(8))
        );
        // Without a width the rule stays a fixed 40 columns.
        assert_eq!(plain("---"), format!("{}\n", "─".repeat(40)));
    }

    #[test]
    fn width_parsing_rejects_bad_values() {
        assert_eq!(super::parse_width("40").unwrap(), 40);
        assert!(super::parse_width("0").is_none());
        assert!(super::parse_width("abc").is_none());
    }
}
