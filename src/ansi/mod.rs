//! Turn a `tmux capture-pane -e` snapshot into styled ratatui [`Line`]s, so the
//! dashboard's live-pane view shows the agent's OWN colours instead of flat white.
//!
//! A FAITHFUL MIRROR, deliberately: the agent's colours go out exactly as it emitted
//! them. No normalisation, no theme remapping, no palette of our own — what the human
//! sees here is what they would see attached to the pane.
//!
//! ## Two decisions a reviewer will ask about
//!
//! **This is the first place `ratatui` appears in the LIBRARY's public API** (it was a
//! `pmtui`-only dependency before). Deliberate: `tests/integration/` cannot import a
//! bin crate, and the acceptance test for this feature has to run the parser over bytes
//! a REAL tmux produced. `ratatui` was already a normal dependency, so no new crate
//! enters the tree. `stream_json` is the precedent by role: a pure presentation
//! transform that the TUI calls and the tests can reach.
//!
//! **"pmtui uses BASIC ANSI colours only" still holds.** That rule is about pmtui's OWN
//! CHROME — its glyphs, chips and status bar stay in the 8 basic slots so the user's
//! terminal theme wins. It never said pmtui may not ECHO an agent's colours, which are
//! the agent's content, not our decoration. Nor is indexed SGR new on the wire:
//! `Color::Red` already leaves pmtui as `ESC[38;5;1m` (ratatui → crossterm maps every
//! named colour through the 256-colour form), so emitting `Color::Indexed`/`Color::Rgb`
//! here adds no compatibility risk that day one did not already carry.
//!
//! ## What `capture-pane -e` actually contains
//!
//! Measured over ~70KB of real captures from `claude` v2.1.x and `codex` v0.14.x. Only
//! three escape forms occur:
//!
//! 1. **CSI ending in `m` (SGR)** — and NOTHING else. Every one of 3272 observed CSIs
//!    was an SGR: no cursor moves, no erases, no `?25l`. That is STRUCTURAL, not luck:
//!    `-e` serialises tmux's CELL GRID, it does not replay the byte stream. tmux already
//!    consumed every cursor move and erase while building the grid, so `H`/`A`/`K`/`J`
//!    *cannot* come back out. Non-`m` finals are still parsed and skipped, because
//!    "cannot happen" is a reason to not depend on it, not a reason to panic on it.
//! 2. **OSC 8 hyperlinks** (always ST-terminated), wrapping filenames inside ordinary
//!    tool and dialog rows — not just the welcome banner. This is a TMUX feature (3.4+
//!    stores hyperlinks in the grid and re-emits them), so it is not a claude quirk and
//!    `codex` having none today is state-dependent. Handled unconditionally: the wrapper
//!    is stripped and the link TEXT kept, because leaving it in splatters
//!    `file:///…` URLs across the pane — strictly worse than the white text we are fixing.
//! 3. **ST (`ESC \`)** — only ever as the OSC 8 terminator.
//!
//! Per engine: `claude` emits one attribute per SGR (`fg`+`bg` is two adjacent escapes),
//! closes bold with a full `0m` and never a selective `22m`, and uses `38;5;N`/`48;5;N`
//! indexed colour with no truecolor, italic or underline. `codex` differs and is
//! supported too: compound multi-parameter SGR (`0;1m`, `1;2m`), `3m` italic, and
//! truecolor `38;2;R;G;B` on its status line. Because `claude` never emits a selective
//! clear, **`0m` has to reset fg, bg AND every modifier** or its bold runs never end.
//!
//! ## The finding that shapes the parser: style carries ACROSS lines
//!
//! `-e` is a DELTA STREAM over the whole capture, not a self-contained run per line. A
//! long coloured run emits its SGR on the first line and NOTHING on the wrapped
//! continuation lines; the closing `39m` may land two lines later. Measured: 125 of 2224
//! SGR-bearing lines ended with style still open. So [`styled_lines`] carries one style
//! accumulator across line boundaries — a per-line-reset parser renders wrapped lines
//! WHITE, which is exactly the bug this module exists to fix. It matters more in
//! production than in any probe: `TmuxDriver::launch_interactive` creates panes with no
//! `-x`/`-y`, so they are 80x24 and wrap sooner than the 120-column probes did.
//!
//! ## Guarantees
//!
//! PURE and TOTAL. No I/O and no environment sniffing — in particular colour depth is
//! NOT probed (`available_color_count()` and friends), because that would make the
//! output machine-dependent and the unit tests would assert different colours on
//! different hosts. `Color::Indexed`/`Color::Rgb` go out as-is and crossterm plus the
//! terminal do the downgrading. Nothing here panics: every adversarial input (a capture
//! cut mid-escape, an unterminated OSC, `ESC[38;5;` with no index, `38;5;999`, `ESC[m`,
//! a lone `ESC`, text that merely looks like an escape) degrades toward RENDERING THE
//! TEXT and dropping only the malformed control bytes.
//!
//! Zero-copy: a [`Span`] holds a `Cow<'a, str>`, so every text run BORROWS the capture
//! rather than copying it. `PreviewLog::Pane(String)` owns the capture and is matched by
//! reference, so the borrow outlives the render.
//!
//! ## The map of this directory
//!
//! One file, because the parser is one state machine: the style accumulator carried
//! across rows is shared by every stage, so there is no seam to split it on. `tests` is
//! the only part that lives apart, and it is as long as the parser it pins — each case
//! there is a shape a REAL `capture-pane -e` produced, so the file doubles as the record
//! of what the two engines emit.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// The escape byte that introduces every sequence handled here.
const ESC: u8 = 0x1b;

/// SGR `0` (and the parameterless `ESC[m`): a FULL reset.
///
/// `fg`/`bg` become the explicit `Color::Reset` ("the terminal's default") rather than
/// `None` ("inherit whatever is underneath"), and no modifier is carried. Modifiers are
/// tracked only in `add_modifier` — never `sub_modifier` — which is sound because each
/// span's style is applied to FRESH cells whose modifier set starts empty, so "not
/// added" already means "off".
const SGR_RESET: Style = Style::new().fg(Color::Reset).bg(Color::Reset);

/// SGR `30`–`37` / `40`–`47`: the 8 standard colours, in code order.
const BASIC: [Color; 8] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
];

/// SGR `90`–`97` / `100`–`107`: the bright variants. ratatui spells bright black
/// `DarkGray` and bright white `White` (its `Gray` is plain white, code 37).
const BRIGHT: [Color; 8] = [
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// Parse a `capture-pane -e` snapshot into one [`Line`] per pane row, with the agent's
/// own colours and attributes attached as [`Span`] styles.
///
/// Row count matches `str::lines()`: a trailing newline does not add a phantom row, and
/// an interior row that holds only escapes becomes an empty (blank) [`Line`] so the
/// caller's "one pane row == one screen row" budget stays exact.
///
/// The style accumulator is carried ACROSS rows — see the module docs; that is the whole
/// reason this is not a per-line parser.
pub fn styled_lines(capture: &str) -> Vec<Line<'_>> {
    let bytes = capture.as_bytes();
    let mut out: Vec<Line> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    // Carried across every row on purpose. See the module docs.
    let mut style = Style::default();
    // Start of the pending run of ordinary text, flushed whenever the style changes,
    // a row ends, or escape bytes have to be dropped.
    let mut run = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                // Drop a CR directly before the LF so a `\r\n` row leaves no stray
                // control cell. tmux emits a bare `\n`; this is belt-and-braces.
                let end = if i > run && bytes[i - 1] == b'\r' {
                    i - 1
                } else {
                    i
                };
                push_run(&mut spans, capture, run, end, style);
                out.push(Line::from(std::mem::take(&mut spans)));
                i += 1;
                run = i;
            }
            ESC => {
                push_run(&mut spans, capture, run, i, style);
                i = consume_escape(capture, bytes, i, &mut style);
                run = i;
            }
            _ => i += 1,
        }
    }
    // The final row is only emitted when it actually holds text: a capture ending in a
    // newline has no trailing row, and a trailing escape fragment is not one either.
    push_run(&mut spans, capture, run, bytes.len(), style);
    if !spans.is_empty() {
        out.push(Line::from(spans));
    }
    out
}

/// Does `capture` hold any VISIBLE text once escape sequences are stripped?
///
/// The render path needs this to decide whether a live pane is worth showing, and it
/// must NOT be `capture.trim().is_empty()`: escape bytes are not whitespace, so a pane
/// holding nothing but its colour state looks non-empty to `trim` and the preview then
/// claims the agent has output when the screen would be blank. Defined in terms of
/// [`styled_lines`] so the two can never disagree about what counts as text.
pub fn has_visible_text(capture: &str) -> bool {
    styled_lines(capture).iter().any(|l| !line_is_blank(l))
}

/// A row with nothing on it but whitespace — the shape a pane taller than its content
/// ends in, which the caller trims so the tail sits flush on the bottom of its region.
pub fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.trim().is_empty())
}

/// Push `capture[start..end]` as one styled span, skipping an empty run.
fn push_run<'a>(
    spans: &mut Vec<Span<'a>>,
    capture: &'a str,
    start: usize,
    end: usize,
    style: Style,
) {
    if end <= start {
        return;
    }
    // `start` and `end` always sit on an ASCII byte (`ESC`, `\n`, `\r`, or the end of
    // the string) and every UTF-8 continuation byte is >= 0x80, so the range is always
    // on char boundaries. `get` keeps this total regardless.
    if let Some(text) = capture.get(start..end) {
        spans.push(Span::styled(text, style));
    }
}

/// Consume the escape sequence starting at `esc` (which holds [`ESC`]), updating `style`
/// for an SGR, and return the index of the first byte AFTER it.
///
/// Always returns `> esc`, so the caller's scan cannot stall.
fn consume_escape(capture: &str, bytes: &[u8], esc: usize, style: &mut Style) -> usize {
    match bytes.get(esc + 1) {
        Some(b'[') => consume_csi(capture, bytes, esc, style),
        Some(b']') => consume_osc(bytes, esc),
        // A stray ST (`ESC \`) with no OSC open, or any other two-byte escape
        // (charset/keypad selection — proven absent from `-e` output): drop both bytes.
        Some(_) => esc + 2,
        // A lone trailing `ESC`: the capture was cut mid-escape. Drop the one byte.
        None => esc + 1,
    }
}

/// Consume a CSI (`ESC [` … final) and apply it when the final byte is `m` (SGR).
fn consume_csi(capture: &str, bytes: &[u8], esc: usize, style: &mut Style) -> usize {
    let params_start = esc + 2;
    let mut j = params_start;
    while let Some(&b) = bytes.get(j) {
        match b {
            // Parameter bytes (digits, `;`, `:`, and the private-use `<=>?`) and
            // intermediates (` `..`/`): keep scanning for the final byte.
            0x20..=0x3f => j += 1,
            // The final byte. `m` is SGR — the only CSI `-e` can produce (module docs).
            // Any other well-formed final is consumed and ignored rather than shown.
            0x40..=0x7e => {
                if b == b'm' {
                    apply_sgr(style, capture.get(params_start..j).unwrap_or(""));
                }
                return j + 1;
            }
            // A control byte (typically the next `\n`) inside a CSI: malformed. Drop
            // only what we scanned and leave this byte for the caller to re-read, so a
            // truncated escape can never eat the following row.
            _ => return j,
        }
    }
    // Ran off the end of the capture — cut mid-escape. Drop the fragment; there is no
    // user text in it, only parameters.
    j
}

/// Consume an OSC (`ESC ]` … `ESC \` / BEL) and drop it whole.
///
/// Only OSC 8 (hyperlinks) was observed and it is always ST-terminated, but every OSC is
/// treated alike: an OSC payload is never printable text, so stripping the wrapper and
/// keeping what follows is right for all of them. An UNTERMINATED OSC gives up at the
/// row boundary rather than swallowing the rest of the pane — at worst the tail of one
/// row is lost, never the rows below it.
fn consume_osc(bytes: &[u8], esc: usize) -> usize {
    let mut j = esc + 2;
    while let Some(&b) = bytes.get(j) {
        match b {
            // BEL terminator (tmux uses ST, but a BEL-terminated OSC is legal).
            0x07 => return j + 1,
            // ST — the terminator tmux actually writes.
            ESC if bytes.get(j + 1) == Some(&b'\\') => return j + 2,
            b'\n' => return j,
            _ => j += 1,
        }
    }
    j
}

/// Apply one SGR's parameter list (the text between `ESC[` and `m`) to `style`.
///
/// Compound lists are handled parameter by parameter, left to right, because `codex`
/// emits them (`0;1m`, `1;2m`) even though `claude` never does. An unknown parameter is
/// IGNORED — blink, conceal and overline are deliberately unmapped: neither engine emits
/// them, and ignoring one costs an attribute while honouring `8m` (conceal) would hide
/// text we were asked to show.
fn apply_sgr(style: &mut Style, params: &str) {
    // `ESC[m` — no parameters at all — is SGR 0.
    if params.is_empty() {
        *style = SGR_RESET;
        return;
    }
    let mut it = params.split(';');
    while let Some(raw) = it.next() {
        let Some(code) = parse_param(raw) else {
            continue;
        };
        match code {
            0 => *style = SGR_RESET,
            1 => style.add_modifier.insert(Modifier::BOLD),
            2 => style.add_modifier.insert(Modifier::DIM),
            3 => style.add_modifier.insert(Modifier::ITALIC),
            4 => style.add_modifier.insert(Modifier::UNDERLINED),
            7 => style.add_modifier.insert(Modifier::REVERSED),
            9 => style.add_modifier.insert(Modifier::CROSSED_OUT),
            // 22 is "normal intensity": ONE attribute covering both bold and dim.
            22 => style.add_modifier.remove(Modifier::BOLD | Modifier::DIM),
            23 => style.add_modifier.remove(Modifier::ITALIC),
            24 => style.add_modifier.remove(Modifier::UNDERLINED),
            27 => style.add_modifier.remove(Modifier::REVERSED),
            29 => style.add_modifier.remove(Modifier::CROSSED_OUT),
            30..=37 | 90..=97 => {
                if let Some(c) = basic_color(code) {
                    style.fg = Some(c);
                }
            }
            // The `5;N` / `2;R;G;B` tail is read off the SAME iterator, so the outer
            // loop resumes after the colour's parameters.
            38 => {
                if let Some(c) = extended_color(&mut it) {
                    style.fg = Some(c);
                }
            }
            39 => style.fg = Some(Color::Reset),
            40..=47 | 100..=107 => {
                if let Some(c) = basic_color(code) {
                    style.bg = Some(c);
                }
            }
            48 => {
                if let Some(c) = extended_color(&mut it) {
                    style.bg = Some(c);
                }
            }
            49 => style.bg = Some(Color::Reset),
            _ => {}
        }
    }
}

/// One SGR parameter as a number.
///
/// An EMPTY parameter is the ECMA-48 default `0` (so `ESC[;m` resets twice). `None` for
/// anything that is not a plain decimal number — notably the `38:5:N` colon sub-parameter
/// form (proven absent from both engines) and any value too large for `u32` — so it is
/// skipped rather than mis-read into some other attribute.
fn parse_param(raw: &str) -> Option<u32> {
    if raw.is_empty() {
        return Some(0);
    }
    raw.parse::<u32>().ok()
}

/// A basic-palette fg/bg code as a named colour. `None` for anything outside the four
/// basic ranges (unreachable from [`apply_sgr`]'s match arms, but kept total).
fn basic_color(code: u32) -> Option<Color> {
    let (offset, table) = match code {
        30..=37 => (code - 30, &BASIC),
        40..=47 => (code - 40, &BASIC),
        90..=97 => (code - 90, &BRIGHT),
        100..=107 => (code - 100, &BRIGHT),
        _ => return None,
    };
    table.get(usize::try_from(offset).ok()?).copied()
}

/// The `5;N` (256-colour) or `2;R;G;B` (truecolor) tail of a `38`/`48` parameter, read
/// off the shared parameter iterator.
///
/// `None` — leaving the colour untouched — for a truncated list (`38;5` with no index),
/// an out-of-`u8` component (`38;5;999`) or an unknown selector, so a malformed colour
/// costs the colour and never the text. Truecolor is emitted as `Color::Rgb` verbatim;
/// it is config-gated in `codex` (`[tui] status_line_use_colors`), so its PRESENCE is
/// never something to depend on.
fn extended_color<'a>(it: &mut impl Iterator<Item = &'a str>) -> Option<Color> {
    match parse_param(it.next()?)? {
        5 => Some(Color::Indexed(component(it.next()?)?)),
        2 => Some(Color::Rgb(
            component(it.next()?)?,
            component(it.next()?)?,
            component(it.next()?)?,
        )),
        _ => None,
    }
}

/// One colour component (a 256-colour index or an RGB channel), which must be spelled
/// out and fit in a `u8`.
///
/// Deliberately NOT [`parse_param`]: an EMPTY colour component is rejected instead of
/// defaulting to ECMA-48's `0`. `0` is black, and on the dark background this renders
/// against a defaulted black would make the text INVISIBLE — worse than the flat white
/// this module replaces. Dropping a malformed colour leaves the text readable.
fn component(raw: &str) -> Option<u8> {
    raw.parse::<u8>().ok()
}

#[cfg(test)]
mod tests;
