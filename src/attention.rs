//! How loud a row is allowed to be, and the glyphs and styles that say so.
//!
//! ONE place holds the whole dashboard palette. Before this module the answer was
//! spread across `project_row_line`, `render_status_bar`, `stop_preview_lines` and
//! `wake_state_color`, each inventing its own literal — which is how a `Stuck` row
//! came to draw a red `✕` above a yellow `!!` badge, the glyph and the badge
//! disagreeing about severity on the same row (see [`Level`]).
//!
//! # The colour rule, restated precisely
//!
//! Chrome may use the sixteen NAMED [`Color`] variants, as foreground or as
//! background, plus any [`Modifier`]. Those land on palette indices 0–15 — exactly
//! the slots a terminal theme defines — so the user's own theme picks the actual
//! hue. `Color::Indexed(n)` for `n >= 16` and `Color::Rgb` are forbidden in chrome
//! because they bypass the theme and pin an absolute colour onto an unknown
//! background. (The Log pane is the deliberate exception: it MIRRORS the agent's own
//! bytes, so `crate::ansi` reproduces whatever the agent emitted, indexed and RGB
//! included. Nothing here touches that.)
//!
//! # Backgrounds are reached through `REVERSED`, not `.bg()`
//!
//! Two independent reasons, both measured:
//!
//! 1. **`.bg()` inverts under selection.** The list's `highlight_style` is applied
//!    AFTER the row's own spans (`ratatui-widgets`'s `list::rendering` calls
//!    `buf.set_style(row_area, highlight_style)`, and `Cell::set_style` does
//!    `modifier.insert`). An explicit `fg(Black).bg(Red)` fill would therefore flip
//!    to red-text-on-black exactly when the human selects the row.
//! 2. **It cannot get the pairing wrong.** `REVERSED` paints the field in the fg
//!    colour and the glyphs in the terminal's OWN background colour — a pair the
//!    theme author already chose to be readable together.
//!
//! What `REVERSED` does NOT do is improve contrast. Contrast is symmetric, so
//! `fg(X) + REVERSED` has exactly the ratio of plain `fg(X)` on the same
//! background: reverse video buys AREA, not ratio. Measured minima across ten
//! popular dark themes (sRGB relative luminance, theme fg/bg pairs): bare
//! `REVERSED` 4.75, Yellow 4.68, Green 4.69, **Red 2.69** (Gruvbox; 2.71 on the
//! linux console, 3.05 on Nord).
//!
//! That last number is why [`fill_hard`] is built the way it is. Red is the one hue
//! that cannot carry text on its own on EVERY theme, so on a Hard row it is never the
//! only carrier: the `✕` glyph, the word `stuck`/`needs you`, and the `!!!` badge all
//! say the same thing, and — deliberately — the session **id keeps the theme's own
//! foreground** (bold, not hued), so identifying WHICH session is shouting never
//! depends on a low-contrast colour.
//!
//! # Tuned against Catppuccin Mocha
//!
//! The palette this UI was tuned on, measured (relative luminance, against that
//! theme's own `#1e1e2e` background):
//!
//! | idx | name       | hex       | vs bg |
//! |-----|------------|-----------|-------|
//! | 0   | Black      | `#45475a` |  1.80 |
//! | 1   | Red        | `#f38ba8` |  7.08 |
//! | 2   | Green      | `#a6e3a1` | 11.03 |
//! | 3   | Yellow     | `#f9e2af` | 12.91 |
//! | 4   | Blue       | `#89b4fa` |  7.79 |
//! | 5   | Magenta    | `#f5c2e7` | 10.74 |
//! | 6   | Cyan       | `#94e2d5` | 11.01 |
//! | 8   | DarkGray   | `#585b70` |  2.46 |
//! | 15  | White      | `#bac2de` |  9.26 |
//!
//! Two things follow, and both shaped the design:
//!
//! 1. **Index 0 is not black — it is LIGHTER than the background** (1.80). Catppuccin
//!    calls it `surface1` and uses it for raised panels, which is exactly what
//!    [`surface`] uses it for. An overlay is a lighter surface INSIDE its border, the way
//!    a card is — the border draws the edge, the surface makes it a card rather than a
//!    hole punched in the dashboard.
//! 2. **Every hue clears 7:1 here**, red included, so on this theme hue can be trusted
//!    and the redundancy above is belt-and-braces rather than load-bearing. The
//!    conservative rule stays anyway, because a terminal's palette is the user's to
//!    change and Gruvbox's red really is 2.69.
//!
//! `DarkGray` at 2.46 is deliberately reserved for things that must recede — pane
//! borders, section rules, de-emphasised text — and is never asked to carry a fact on
//! its own.
//!
//! No blink. tmux normalises rapid blink (`SGR 6`) to slow (`SGR 5`), terminal
//! honouring is inconsistent, and it is a WCAG 2.2.2 / 2.3.1 hazard.

use ratatui::style::{Color, Modifier, Style};

use crate::policy;
use crate::state::RiskClass;
use crate::view::{Posture, ProjectView};

/// How much visual force a row has earned.
///
/// Derived from a [`ProjectView`] and nothing else, so it is a pure function of what
/// is on disk and testable without a terminal.
///
/// The split between [`Level::Hard`] and [`Level::Soft`] is also a bug fix. It used
/// to be computed from `v.stops` alone, but a `Stuck` row reached through the
/// driver's `Timeout`/repeated-failure path has NO stops (see
/// `view::derive_posture`), so such a row drew the red `✕` of the stuck bucket above
/// the yellow `!!` badge of the medium one. Folding `Stuck` into `Hard` closes that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Wedged, or waiting on a decision the harness refuses to make for you.
    Hard,
    /// Waiting on you, but nothing about it is irreversible.
    Soft,
    /// Working right now. Needs nothing — and so is deliberately NOT filled.
    Live,
    /// Fresh, monitoring, idle, done. The resting state, and most rows most of
    /// the time.
    Calm,
    /// `enabled: false` — programmatically disabled. Drops the fill (a whole-row
    /// `DIM` stacked on a `BOLD` fill emits `\e[1m\e[2m`, whose intensity is
    /// implementation-defined) and keeps the hue.
    Off,
}

/// [`Level`] for one row.
///
/// Ordered to agree with `status_category`'s four-bucket partition, which the
/// header counts and the row glyphs share: `Stuck` is tested BEFORE
/// `needs_attention()`, which is true for both. `Off` shadows everything, because a
/// disabled row cannot be acted on.
pub fn level(v: &ProjectView) -> Level {
    if !v.enabled {
        return Level::Off;
    }
    level_active(v)
}

/// [`level`] as if the row were enabled.
///
/// A disabled row still needs the level UNDERNEATH its `Off`, because it keeps its
/// hue and its badge — dropping only the fill. Splitting this out is what keeps
/// `Off` from having to mean two things at once.
pub fn level_active(v: &ProjectView) -> Level {
    if v.posture == Posture::Stuck {
        return Level::Hard;
    }
    if v.posture.needs_attention() {
        return if has_hard_stop(v) {
            Level::Hard
        } else {
            Level::Soft
        };
    }
    if matches!(v.posture, Posture::Running | Posture::Working) {
        return Level::Live;
    }
    Level::Calm
}

/// Does any open stop escalate to [`RiskClass::Hard`]?
///
/// `policy::effective_risk` — not the stop's declared risk — because
/// `Publish`/`Merge`/`ConfirmDone`/`Stuck`/`Capability` are forced Hard regardless
/// of what the agent asked for.
pub fn has_hard_stop(v: &ProjectView) -> bool {
    v.stops
        .iter()
        .any(|s| policy::effective_risk(s) == RiskClass::Hard)
}

impl Level {
    /// Does this level want a filled (reverse-video) label?
    pub fn is_filled(self) -> bool {
        matches!(self, Level::Hard | Level::Soft)
    }

    /// The hue this level speaks in, or `None` for the levels that don't have one.
    pub fn hue(self) -> Option<Color> {
        match self {
            Level::Hard => Some(crate::theme::hard()),
            Level::Soft => Some(crate::theme::soft()),
            Level::Live => Some(crate::theme::live()),
            Level::Calm | Level::Off => None,
        }
    }

    /// The style for this level's SECONDARY LABEL — the row's `needs you` / `stuck` /
    /// `working` column, and the preview's matching badge.
    ///
    /// This is where "background colour on text" lands, and it costs zero columns:
    /// the label is already there and already 14 wide.
    pub fn label_style(self) -> Style {
        match self {
            Level::Hard => fill_hard(),
            Level::Soft => fill_soft(),
            Level::Live => text_live(),
            Level::Calm => text_dim(),
            // A disabled row is dimmed whole-row by the caller, and `DIM` over a `BOLD`
            // fill is implementation-defined — so `Off` keeps the hue and drops the fill.
            Level::Off => text_dim(),
        }
    }
}

// There was a `Level::badge() -> Option<&str>` here, giving `!!!` for Hard and `!! ` for Soft.
// REMOVED at the user's word (*"i want to remove !!! from needs you"*), and deleted rather than
// left unused so nothing looks like a feature that is merely unwired.
//
// It was the row's only non-colour carrier of hard-vs-soft. That distinction now lives where a
// human actually acts on it: `stop_preview_lines` gives every stop a `hard`/`medium` chip in the
// preview, and the `a` overlay repeats it per option. `Level::hue` still separates the two on the
// row (red vs yellow fill), which is emphasis rather than the whole message.

/// The secondary label's style for one row, `Off` included.
///
/// A DISABLED row keeps the hue of the level underneath and drops the fill: the
/// caller dims the whole row, and `DIM` stacked on a `BOLD` reverse-video field emits
/// `\e[1m\e[2m`, whose text intensity is implementation-defined — so a disabled row
/// with a fill could render brighter than an active one.
pub fn label_style_for(v: &ProjectView) -> Style {
    let lvl = level(v);
    if lvl != Level::Off {
        return lvl.label_style();
    }
    match level_active(v).hue() {
        Some(c) => Style::default().fg(c),
        None => text_dim(),
    }
}

/// Waiting on a human, and irreversible or undecidable: a red field, bold.
///
/// See the module docs for why this is `REVERSED` rather than `.bg(Color::Red)`, and
/// why red's 2.69 floor is acceptable HERE and nowhere else.
pub fn fill_hard() -> Style {
    crate::theme::fill(crate::theme::hard())
}

/// Waiting on a human, reversibly: a yellow field, bold. 4.68 worst case.
pub fn fill_soft() -> Style {
    crate::theme::fill(crate::theme::soft())
}

/// Working. Plain hued text on the theme's own background — the pair the theme
/// tuned. Deliberately NOT filled: a fill means "act on me", and a working agent
/// needs nothing. It replaces a `DIM` with no foreground at all, which rendered
/// "working" and "done" identically.
pub fn text_live() -> Style {
    Style::default().fg(crate::theme::live())
}

/// The resting style for secondary text. Unchanged from what shipped.
pub fn text_dim() -> Style {
    Style::default()
        .fg(crate::theme::dim())
        .add_modifier(Modifier::DIM)
}

/// The ` pmtui ` brand chip, and any other label that wants to read as a chip
/// without claiming a severity.
///
/// Replaces `fg(White).bg(Blue)`, which measured **1.06:1** on Catppuccin Mocha,
/// 1.56 on TokyoNight and 2.34 on Nord — i.e. the loudest pixels on the screen were
/// spent on a label that was, on most dark themes, invisible.
pub fn chip_brand() -> Style {
    crate::theme::fill(crate::theme::brand())
}

/// The dashboard's own base pair: the theme's canvas and its ordinary ink.
///
/// Painted over the whole frame before anything draws, and again over any rect a widget has `Clear`ed.
/// `Clear` resets cells to the TERMINAL's default pair, and ratatui PATCHES styles rather than
/// replacing them, so plain text over a cleared rect would otherwise inherit the terminal's ink rather
/// than the theme's — invisible where the two disagree, which is every light theme in a dark terminal.
pub fn canvas() -> Style {
    Style::default()
        .bg(crate::theme::base_bg())
        .fg(crate::theme::text())
}

/// A RAISED PANEL: palette index 0.
///
/// On Catppuccin Mocha that is `surface1` `#45475a`, 1.80 against the background — the
/// colour the theme itself uses for cards and popups, so a filled panel reads as
/// floating above the dashboard. This is what replaced the overlay borders.
///
/// It degrades honestly rather than dangerously: on a theme whose index 0 IS the
/// background the panel simply goes flat, and the modal's edge is carried by its BORDER,
/// which is drawn regardless. That is also why the border is not optional.
///
/// Carries the theme's INK as well as its background, for the reason [`canvas`] does.
pub fn surface() -> Style {
    // FOREGROUND TOO. An overlay starts with `Clear`, which resets every cell to the terminal's own
    // default pair, so the card's own style is the only thing between unstyled text inside it and
    // whatever colour the terminal happens to use — white on a light theme's card, for instance.
    // Naming both sides here themes every overlay's plain text at one stroke, and a span that wants its
    // own hue still overrides it.
    Style::default()
        .bg(crate::theme::surface_bg())
        .fg(crate::theme::text())
}

/// Structural lines that must recede: pane borders and section rules.
///
/// Pane borders used to be drawn in the theme's full-strength FOREGROUND (11.34 on
/// Mocha), which made the frame the loudest thing on a screen whose whole job is to
/// show what is inside it. At 2.46 the line is still legible as structure and stops
/// competing with content.
pub fn rule() -> Style {
    Style::default().fg(crate::theme::rule())
}

/// A pane's own title (` SESSIONS `, ` PREVIEW `). Blue, bold — an accent that names
/// the region without shouting, and the one place the dashboard uses index 4.
pub fn pane_title() -> Style {
    Style::default()
        .fg(crate::theme::accent())
        .add_modifier(Modifier::BOLD)
}

/// The SELECTED row: a raised surface, like [`surface`].
///
/// It was `UNDERLINED`, which was chosen only because the previous `REVERSED` collided
/// with the attention fills. A raised background collides with nothing (a `REVERSED`
/// span inside it still draws as a hued block, just with the surface as its text
/// colour) and reads far better than an underline under every span of a row.
///
/// The `▸ ` gutter remains the PRIMARY indicator, which is what makes this safe on a
/// theme whose index 0 equals the background.
pub fn selection() -> Style {
    Style::default().bg(crate::theme::selection_bg())
}

/// Ordinary foreground text — the colour an id, a label or a value wears when it is stating itself
/// rather than a state. Re-exported from [`crate::theme`] so the render tree keeps reaching the
/// palette through ONE module.
pub fn text() -> Color {
    crate::theme::text()
}

/// The four glanceable bucket glyphs — `status_category`'s partition, in its order:
/// `0` needs-you, `1` running, `2` idle/done, `3` stuck.
///
/// Every bucket has its own SHAPE, not just its own hue, because "the agent is
/// wedged" is not the same call to action as "the agent asked you something" and a
/// reader who cannot separate red from yellow still has to be able to tell them
/// apart.
pub fn bucket_glyph(cat: u8) -> (&'static str, Color) {
    match cat {
        0 => ("◐", crate::theme::soft()),
        1 => ("●", crate::theme::live()),
        3 => ("✕", crate::theme::hard()),
        _ => ("○", crate::theme::rule()),
    }
}

/// Selection gutter. TWO columns, and that is load-bearing: `sessions_width`'s
/// documented floor counts 2 for the marker and `render_sessions` subtracts them in
/// `content_w` (`area.width - 8`: 2 borders + 4 inner padding + 2 marker), while ratatui
/// derives the item area from `highlight_symbol.width()`. A one-column marker would
/// silently desynchronise those two.
pub const GUTTER: &str = "▸ ";

/// "This has been waiting for you" (preview). U+29D7 — one column, and carries no
/// Unicode `Emoji` property, unlike the tempting `⏳`, which font fallback draws two
/// cells wide.
pub const WAITING: &str = "⧗";

/// Low-risk marker in the preview's stop list. U+00B7.
pub const LOW_RISK: &str = "·";

/// Every glyph this module hands to the renderer.
///
/// Pinned one column wide by `every_glyph_is_one_column`. That test borrows
/// ratatui's own `unicode-width` tables (no new dependency), which is exactly as far
/// as it can go: `◐ ● ○ ·` are East-Asian-Width **Ambiguous**, so a terminal
/// explicitly configured to render ambiguous-width characters WIDE would still draw
/// them in two cells. `⧗` and `▸` are Neutral. What the test does close is the
/// regression that actually happened — a glyph whose display width exceeds its char
/// count, making `text_cols` undercount and the truncator evict a chip.
pub const GLYPHS: &[&str] = &["◐", "●", "○", "✕", "⧗", "·", "▸"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Mode;
    use crate::state::{Stop, Tier};
    use ratatui::text::Span;

    fn stop(kind: &str, risk_class: RiskClass) -> Stop {
        Stop {
            id: format!("stop-{kind}"),
            kind: kind.into(),
            risk_class,
            question: String::new(),
            options: Vec::new(),
            context_ref: None,
            status: "open".into(),
        }
    }

    fn view(posture: Posture) -> ProjectView {
        ProjectView {
            id: "bot".into(),
            display_name: None,
            work_summary: None,
            project_name: None,
            forked_from: None,
            incomplete_fork: false,
            spawned_by: None,
            spawned_by_label: None,
            spawn_staged: false,
            job: false,
            job_commit: None,
            job_branch: None,
            enabled: true,
            tier: Some(Tier::Autopilot),
            posture,
            next_action: String::new(),
            step_id: 1,
            last_activity: None,
            stops: Vec::new(),
            oldest_stop_since: None,
            mode: Mode::AgentLoop,
            engine: None,
            session_live: false,
            human_attached: false,
            agent_working: None,
            autopilot_events: Vec::new(),
            turn_trace: Vec::new(),
            decision_digest: crate::job::DecisionCounters::default(),
            advice_inflight: None,
            decider_live: false,
            advice_queue: Vec::new(),
            decider_runs: Vec::new(),
            decider_engine: Some(crate::registry::Engine::Claude),
            decider_model: None,
        }
    }

    #[test]
    fn every_glyph_is_one_column() {
        for g in GLYPHS {
            assert_eq!(
                Span::raw(*g).width(),
                1,
                "{g:?} is not one display column — `text_cols` would undercount it \
                 and the row truncator would evict a trailing chip"
            );
            assert_eq!(g.chars().count(), 1, "{g:?} is more than one char");
        }
    }

    #[test]
    fn level_is_hard_for_stuck_with_no_stops() {
        // The bug this module exists to close: a driver-Timeout `Stuck` row carries no
        // stops at all, so a stops-only derivation called it Soft and painted the SOFT
        // yellow fill under the stuck bucket's red `✕` — the glyph and the fill
        // disagreeing about severity on one row.
        let v = view(Posture::Stuck);
        assert!(v.stops.is_empty());
        assert_eq!(level(&v), Level::Hard);
        // The HUE is the theme's failure role, not a named ANSI slot: chrome takes every colour
        // from `crate::theme` now, so this asserts the ROLE the level maps to.
        assert_eq!(level(&v).hue(), Some(crate::theme::hard()));
    }

    #[test]
    fn needs_you_is_soft_until_a_stop_escalates_to_hard() {
        let mut v = view(Posture::NeedsYou);
        v.stops = vec![stop("ambiguity", RiskClass::Medium)];
        assert_eq!(level(&v), Level::Soft);
        // `confirm_done` is forced Hard by `policy::effective_risk` no matter what risk
        // the agent declared, so the row must follow the POLICY, not the claim.
        v.stops = vec![stop("confirm_done", RiskClass::Low)];
        assert_eq!(level(&v), Level::Hard);
    }

    #[test]
    fn a_disabled_row_is_off_however_loudly_it_would_otherwise_shout() {
        let mut v = view(Posture::Stuck);
        v.enabled = false;
        assert_eq!(level(&v), Level::Off);
        assert!(!level(&v).is_filled());
        // …but the level underneath survives, so the row keeps its hue.
        assert_eq!(level_active(&v), Level::Hard);
        let s = label_style_for(&v);
        assert_eq!(s.fg, Some(crate::theme::hard()));
        assert!(
            !s.add_modifier.contains(Modifier::REVERSED),
            "a disabled row must not carry a fill under the whole-row DIM"
        );
    }

    #[test]
    fn working_is_live_and_never_filled() {
        assert_eq!(level(&view(Posture::Working)), Level::Live);
        assert_eq!(level(&view(Posture::Running)), Level::Live);
        assert!(!Level::Live.is_filled());
        // …and it finally has a foreground of its own; it used to be `DIM` with none,
        // which is the same rendering as `done`.
        assert_eq!(Level::Live.label_style().fg, Some(crate::theme::live()));
    }

    #[test]
    fn calm_and_idle_are_untouched() {
        assert_eq!(level(&view(Posture::Fresh)), Level::Calm);
        assert_eq!(level(&view(Posture::Monitoring)), Level::Calm);
        assert_eq!(level(&view(Posture::Done)), Level::Calm);
        assert_eq!(Level::Calm.label_style(), text_dim());
    }

    #[test]
    fn no_chrome_style_sets_an_absolute_background() {
        // The whole rule: a background is reached by REVERSING the theme's own pair,
        // never by naming one. A `.bg()` here would invert under the list's
        // `highlight_style` exactly when the row is selected.
        for (name, s) in [
            ("fill_hard", fill_hard()),
            ("fill_soft", fill_soft()),
            ("text_live", text_live()),
            ("text_dim", text_dim()),
            ("chip_brand", chip_brand()),
        ] {
            assert!(s.bg.is_none(), "{name} sets a background colour");
            // Anything meant to read as a field must actually reverse; anything else
            // must not, or plain text would look like a badge.
            let reversed = s.add_modifier.contains(Modifier::REVERSED);
            let is_fill = matches!(name, "fill_hard" | "fill_soft" | "chip_brand");
            assert_eq!(reversed, is_fill, "{name}: REVERSED is wrong for its role");
        }
    }

    // `the_badges_are_the_same_width_so_severity_cannot_be_shortened` lived here. It guarded a
    // real hazard — ` !!!` truncated to ` !!` IS the milder badge — that cannot occur now that
    // there is no badge to truncate.

    #[test]
    fn the_bucket_glyphs_are_the_shipped_partition() {
        // Pinned deliberately: `status_category` in pmtui feeds BOTH the header counts
        // and the row glyphs from this table, and a fifth glyph (say, hard-vs-soft
        // needs-you) would break that one-row-one-bucket invariant.
        assert_eq!(bucket_glyph(0), ("◐", crate::theme::soft()));
        assert_eq!(bucket_glyph(1), ("●", crate::theme::live()));
        assert_eq!(bucket_glyph(2), ("○", crate::theme::rule()));
        assert_eq!(bucket_glyph(3), ("✕", crate::theme::hard()));
        // …and the four are distinguishable, whatever the theme: the partition is only readable
        // if each bucket's hue differs from the others'.
        let hues: std::collections::HashSet<String> =
            (0..4).map(|b| format!("{:?}", bucket_glyph(b).1)).collect();
        assert_eq!(hues.len(), 4, "two buckets share a hue: {hues:?}");
    }
}
