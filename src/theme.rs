//! The active theme, and the ROLES the dashboard paints with.
//!
//! `opaline` resolves a theme in three layers — palette (raw hex) → semantic token
//! (`text.primary`, `accent.primary`, `error`) → style — and ships 38 of them. This module is the
//! seam: it names the roles THIS dashboard has, maps each onto an opaline token, and hands
//! [`crate::attention`] ratatui colours. Nothing else reads a token string, so re-pointing a role at
//! a different token is one edit here rather than a search across the render tree.
//!
//! # What changed, and what it cost
//!
//! Chrome used to be restricted to the sixteen NAMED [`Color`] variants so the user's terminal theme
//! picked every hue, and `Color::Rgb` was forbidden. A theme engine cannot work that way: a theme IS
//! a set of absolute colours. So the rule is now narrower — **RGB enters chrome only through this
//! module** — and the contrast guarantee moves with it: it used to be ours, measured across ten
//! terminal themes, and it is now the theme author's. `attention`'s defence in depth survives
//! unchanged and is what makes that acceptable: a Hard row says so with its `✕` glyph, the word
//! `stuck`, a `!!!` badge AND its fill, so no single low-contrast hue is load-bearing.
//!
//! # The global, and why
//!
//! The active theme lives in opaline's `global-state`, not on `App`. Every palette accessor in
//! `attention` is a free function called from ~90 render sites; threading a `&Theme` through all of
//! them would be a large mechanical change to pay for a value that is set once at startup and again
//! only when a human picks a new one.

use std::sync::RwLock;

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};

/// The theme a fresh dashboard uses.
///
/// Catppuccin Mocha, because `attention`'s contrast notes were measured against exactly that palette
/// — so the default look is a real opaline theme rather than a surprise, and the numbers in those
/// notes still describe what is on screen.
pub const DEFAULT_THEME: &str = "catppuccin-mocha";

/// One theme a human may choose: its id, how to print it, and whether it is dark or light.
///
/// No colours: a dropdown row is its NAME (user: *"we don't need to display color like that. we can
/// just use the name"*). The variant stays because it is a word, not a hue, and it is the one thing
/// that stops a human picking a light theme for a dark terminal by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    /// The kebab-case id that [`apply`] and the settings file use.
    pub id: String,
    /// The theme's own display name, e.g. `Catppuccin Mocha`.
    pub display: String,
    /// `dark` or `light`.
    pub variant: &'static str,
}

/// Every theme that can be applied, in a stable order: the default first, then the rest by id.
///
/// The default leads so the list opens on a known-good choice, and the remainder is sorted rather
/// than left in opaline's embedding order, because a menu whose order shifts with a dependency bump
/// moves the row out from under the human's cursor.
///
/// A theme that does not load is NOT OFFERED. Every row here can therefore be applied, which is the
/// promise a picker has to keep — the alternative is a list whose rows sometimes fail on Enter.
pub fn available() -> Vec<ThemeChoice> {
    let mut all: Vec<ThemeChoice> = opaline::builtins::list_available_themes()
        .into_iter()
        .filter_map(|info| {
            // Loaded, not just listed: a row the dropdown offers is a row `Enter` can apply.
            load(&info.name).ok()?;
            Some(ThemeChoice {
                display: info.display_name,
                variant: match info.variant {
                    opaline::schema::ThemeVariant::Light => "light",
                    _ => "dark",
                },
                id: info.name,
            })
        })
        .collect();
    all.sort_by(|a, b| {
        let rank = |choice: &ThemeChoice| usize::from(choice.id != DEFAULT_THEME);
        rank(a).cmp(&rank(b)).then_with(|| a.id.cmp(&b.id))
    });
    all
}

/// The id of the theme in force, tracked here because opaline's `meta.name` is the DISPLAY name and
/// what the settings file needs to round-trip is the id.
static ACTIVE_ID: RwLock<Option<String>> = RwLock::new(None);

/// Resolve `id` to a theme WITHOUT installing it.
///
/// Split out from [`apply`] so this half — the one that can fail, and the one that decides what the
/// colours are — can be exercised against any theme without changing the palette the rest of the
/// process is drawing with.
fn load(id: &str) -> Result<opaline::theme::Theme> {
    opaline::builtins::load_by_name(id)
        .with_context(|| format!("no theme named {id:?} (see the Settings view for the list)"))
}

/// Make `id` the active theme, or report that no such theme exists.
///
/// Fails LOUDLY, unlike most of this dashboard's read paths: a typo'd theme in the settings file has
/// to reach the human as a status line, because the alternative — silently drawing the default — is
/// a setting that looks applied and is not. Nothing is installed and nothing is recorded on failure,
/// so a bad id leaves the screen exactly as it was.
pub fn apply(id: &str) -> Result<()> {
    opaline::theme::set_theme(load(id)?);
    if let Ok(mut active) = ACTIVE_ID.write() {
        *active = Some(id.to_string());
    }
    Ok(())
}

/// The id of the active theme.
pub fn active() -> String {
    match ACTIVE_ID.read().ok().and_then(|id| id.clone()) {
        Some(id) => id,
        None => DEFAULT_THEME.to_string(),
    }
}

/// One token of the active theme as a ratatui colour.
///
/// Installs [`DEFAULT_THEME`] on the first read if nothing has chosen one yet. LAZY rather than an
/// `init()` every entry point must remember: opaline's global starts on ITS default
/// (`silkcircuit-neon`), so without this a test that renders without booting the dashboard — which is
/// most of them — would measure a palette the product never shows. A human's choice still wins,
/// because `apply` records the id and this only fills the gap when none has been recorded.
fn color(token: &str) -> Color {
    if ACTIVE_ID.read().is_ok_and(|id| id.is_none()) {
        let _ = apply(DEFAULT_THEME);
    }
    opaline::theme::current().color(token).into()
}

/// A failure — a stuck row, a refused key, a driver error.
pub fn hard() -> Color {
    color(opaline::names::tokens::ERROR)
}

/// Something the human must answer, though nothing is broken.
pub fn soft() -> Color {
    color(opaline::names::tokens::WARNING)
}

/// An agent working right now.
pub fn live() -> Color {
    color(opaline::names::tokens::SUCCESS)
}

/// The dashboard's own voice: pane titles, section rules, the view tabs.
pub fn accent() -> Color {
    color(opaline::names::tokens::ACCENT_PRIMARY)
}

/// A second accent, for metadata that must read as distinct from a title without competing with it.
pub fn accent_alt() -> Color {
    color(opaline::names::tokens::ACCENT_SECONDARY)
}

/// The brand chip's fill.
pub fn brand() -> Color {
    color(opaline::names::tokens::ACCENT_TERTIARY)
}

/// Text that is present but not the point.
pub fn muted() -> Color {
    color(opaline::names::tokens::TEXT_MUTED)
}

/// Quieter still: a hint, a placeholder, an inactive tab.
pub fn dim() -> Color {
    color(opaline::names::tokens::TEXT_DIM)
}

/// A border or divider that must recede.
pub fn rule() -> Color {
    color(opaline::names::tokens::BORDER_UNFOCUSED)
}

/// The dashboard's own canvas, painted over every cell before anything else draws.
pub fn base_bg() -> Color {
    color(opaline::names::tokens::BG_BASE)
}

/// The raised background an overlay sits on, so a modal reads as a card rather than a hole.
pub fn surface_bg() -> Color {
    color(opaline::names::tokens::BG_ELEVATED)
}

/// The selected row's background.
pub fn selection_bg() -> Color {
    color(opaline::names::tokens::BG_SELECTION)
}

/// Ordinary foreground text, for the rare span that must state it rather than inherit it.
pub fn text() -> Color {
    color(opaline::names::tokens::TEXT_PRIMARY)
}

/// A filled badge in `hue`: the colour as the FIELD, with the theme's own background showing through
/// the glyphs.
///
/// Reached through `REVERSED` rather than `.bg()` for the two reasons `attention` documents — a
/// `.bg()` flips under the list's `highlight_style`, and reverse video cannot get the fg/bg pairing
/// wrong, because the theme author chose both.
pub fn fill(hue: Color) -> Style {
    Style::default()
        .fg(hue)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

#[cfg(test)]
mod tests;
