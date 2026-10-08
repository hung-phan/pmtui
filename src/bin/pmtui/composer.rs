//! The PROSE buffer behind the Message, goal and directive fields: a terminal-grade text editor,
//! borrowed rather than written.
//!
//! The dashboard's other text surfaces are one-line [`crate::Field`]s — a caret and a `String`,
//! which is all a cadence, a rename or a switcher query is. These three are different in kind: each
//! is prose an agent or the decider will act on, each wants paragraphs, and the human types it with
//! the fingers they use in a shell. User: *"turn the Message component to rich editing like the
//! terminal. So navigation would work with Alt+f/b, etc, Ctrl+J will do new line etc"*, then:
//! *"remember to use the library instead of writing anything yourself"*, then, once it worked:
//! *"if the editor is good, we can apply it to other places for goal and directive as well"*.
//!
//! For the goal and the directive that is more than comfort. Both back a FILE (`brief.md`,
//! `directive.md`) that may hold several lines, and a one-line field could not show one — so those
//! fields used to open EMPTY above a read-only preview, and editing what was already there meant
//! escalating to `$EDITOR`. A real buffer opens on the file itself.
//!
//! So the buffer is [`ratatui_textarea::TextArea`], and this module is a SEAM, not an editor: it owns
//! which library call each key means, the theme the widget draws in, and the `String` the send path
//! wants. Word motion, the kill ring, undo/redo, wrapping and the caret across wrapped rows are the
//! library's — which is the point, because that is the part that is easy to get subtly wrong and
//! `ratatui-textarea` already has it under test upstream.
//!
//! # Why the keys are not simply `TextArea::input`
//!
//! The library's own map is readline's, and it is what we want — except for two keys that belong to
//! the surface: `Enter` SUBMITS (it does not open a line — send the message, save the goal, save the
//! directive) and `Esc` leaves. Those are intercepted here and everything else is handed to the
//! library. A newline therefore needs a chord,
//! and three are accepted because terminals disagree about what they can report: `Ctrl+J` always
//! arrives (crossterm decodes `0x0A` as `Char('j')+CONTROL`), `Alt+Enter` arrives nearly everywhere,
//! and `Shift+Enter` arrives only where the terminal implements the kitty protocol — measured under
//! tmux, where it collapses to a bare `Enter`. Binding all three means the feature never depends on a
//! terminal capability we cannot detect for the human.

use crate::*;
use ratatui::crossterm::event::KeyEvent;
use ratatui_textarea::{CursorMove, Input, TextArea};

/// What an empty field SAYS — the one thing that differs between the three prose surfaces, so it is
/// what [`Composer::seeded`] takes. Each names the newline chord, the single key a human cannot guess
/// in a field where Enter submits.
///
/// The BACKGROUND is deliberately not here. `TextArea` inherits the cells it draws over, so the
/// dashboard's canvas and an overlay's raised card each reach the buffer from the pane or `Block`
/// already painted under it (measured, not assumed: `theming::every_view_paints_every_cell_from_the_theme`
/// and `overlays::an_overlay_is_a_bordered_card_with_a_title_chip` read the cells). A `raised` flag
/// here passed both tests with and without it — state that has to be kept correct at every
/// construction for no observable effect is state that drifts, so it is gone.
pub(crate) type Placeholder = &'static str;

/// The `s` composer: prose for an agent.
pub(crate) const MESSAGE: Placeholder = "message this session \u{b7} Ctrl+J for a new line";
/// The `g` field: the question `brief.md` answers.
pub(crate) const GOAL: Placeholder = "what this session is for \u{b7} Ctrl+J for a new line";
/// The `i` field: the one thing a directive may do.
pub(crate) const DIRECTIVE: Placeholder =
    "a standing rule the decider may only forbid by \u{b7} Ctrl+J for a new line";

/// What a key did to the composer, so the caller knows whether the key is still theirs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// The composer consumed it (typing, a motion, a kill, a newline…).
    Consumed,
    /// `Enter` — deliver the message.
    Send,
    /// `Esc` — park the draft.
    Cancel,
    /// `^X^E` — hand the buffer to `$EDITOR`.
    Editor,
    /// Some OTHER `^X^<letter>` chord, carrying the lowercased letter.
    ///
    /// The composer knows `^X^E` because every buffer has an `$EDITOR`; what else a `^X` chord may
    /// mean belongs to the surface, not to the editor. The directive field uses `^X^R` to RESCIND —
    /// a `directive.md` concept this module has no business holding — so it arrives here as
    /// `Chord('r')` and that handler decides.
    Chord(char),
}

/// The Message buffer: a `TextArea` plus the two bits of state the dashboard adds to it.
#[derive(Clone, Debug)]
pub(crate) struct Composer {
    /// BOXED, because `TextArea` is a large struct and this lives in a [`crate::UiMode`] variant: an
    /// enum sized by its biggest arm would make every mode that big. The indirection is the composer's
    /// own business, so no caller spells it.
    area: Box<TextArea<'static>>,
    /// What this field says when it is empty — see [`Placeholder`].
    placeholder: Placeholder,
    /// `^X` was the last key, so the next one completes a prefix chord.
    ///
    /// `^X^E` is bash's own `edit-and-execute-command`, which is why the human asked for it: *"for the
    /// current Ctrl+e to edit, we can make it more universal like Ctrl+x and Ctrl+e to use vim to
    /// edit"*. It also frees bare `^E` to be readline's end-of-line, which is what a terminal-grade
    /// editor has to mean by it.
    pending_prefix: bool,
}

impl Default for Composer {
    fn default() -> Self {
        Self::from_text(String::new())
    }
}

impl Composer {
    /// A MESSAGE composer holding `text`.
    pub(crate) fn from_text(text: String) -> Self {
        Self::seeded(text, MESSAGE)
    }

    /// A composer holding `text` with the caret at the end — where a human resuming a draft, or
    /// opening a field on the value already on disk, expects it.
    pub(crate) fn seeded(text: String, placeholder: Placeholder) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let mut area = Box::new(TextArea::new(lines));
        // WORD wrapping, because a message is prose: a hard break mid-word in a composer reads as a
        // typo the human did not make.
        area.set_wrap_mode(ratatui_textarea::WrapMode::Word);
        area.remove_line_number();
        area.move_cursor(CursorMove::Bottom);
        area.move_cursor(CursorMove::End);
        let mut composer = Self {
            area,
            placeholder,
            pending_prefix: false,
        };
        composer.restyle();
        composer
    }

    /// The composed message as the send path wants it: one `String`, newlines and all.
    ///
    /// `tmux::sanitize_send_text` keeps `\n`, and `Driver::send_keys` routes anything containing one
    /// through a tmux paste-buffer, so a multi-line message is delivered as ONE input rather than
    /// submitted line by line. That is what makes a multi-line composer worth having.
    pub(crate) fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// How many lines the buffer holds — what a content-driven overlay height asks for.
    pub(crate) fn line_count(&self) -> usize {
        self.area.lines().len()
    }

    /// Whether the buffer holds nothing but whitespace — the test every caller had on a `Field`.
    pub(crate) fn is_empty(&self) -> bool {
        self.text().trim().is_empty()
    }

    /// The caret as `(row, column)`, for parking a draft across a trip through `$EDITOR`.
    pub(crate) fn cursor(&self) -> (usize, usize) {
        let c = self.area.cursor();
        (c.0, c.1)
    }

    /// Put the caret back at `(row, column)`, clamped by the library.
    pub(crate) fn set_cursor(&mut self, row: usize, col: usize) {
        self.area.move_cursor(CursorMove::Jump(
            u16::try_from(row).unwrap_or(u16::MAX),
            u16::try_from(col).unwrap_or(u16::MAX),
        ));
    }

    /// A paste. The library takes the whole string, newlines included.
    pub(crate) fn insert_str(&mut self, text: &str) {
        self.area.insert_str(text);
    }

    /// Put the current theme on the buffer. Called whenever the composer is opened or typed into,
    /// which is the only way a theme can differ from the one a parked draft was written under.
    pub(crate) fn restyle(&mut self) {
        // FOREGROUND ONLY, on purpose: the buffer inherits the pane or card painted under it, so it is
        // one surface with whatever it sits on rather than a rectangle of its own colour.
        self.area.set_style(Style::default().fg(attention::text()));
        self.area
            .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        // No cursor-LINE highlight: one line of a six-line buffer wearing a background reads as a
        // selection, and the caret already says where you are.
        self.area.set_cursor_line_style(Style::default());
        self.area.set_placeholder_text(self.placeholder);
        self.area.set_placeholder_style(attention::text_dim());
    }

    /// The widget to draw.
    ///
    /// BORROWED, NEVER CLONED. `TextArea` caches its screen map and its drawn `Rect` behind
    /// `RefCell`/`Cell`, so rendering through `&self` updates the layout state the next frame and the
    /// scroll depend on. A styled clone per frame (which is what this was at first) threw that cache
    /// away every time and made the view jump after an undo — the buffer had changed but the discarded
    /// map had not. Styles therefore live on the buffer, applied by [`Composer::restyle`].
    pub(crate) fn widget(&self) -> &TextArea<'static> {
        &self.area
    }

    /// Route one key: the dashboard's two keys are ours, the newline chords insert a line, and
    /// EVERYTHING else is the library's readline map.
    pub(crate) fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> Edit {
        // A theme can only change while the composer is closed, so restyling on the way through any
        // key is enough to keep a parked draft from being drawn in the theme it was typed under.
        self.restyle();
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let alt = mods.contains(KeyModifiers::ALT);
        let shift = mods.contains(KeyModifiers::SHIFT);

        // `^X` waits exactly ONE key. Anything that is not another Ctrl+letter cancels the prefix and
        // is then handled normally, so a mistyped chord costs a keystroke rather than eating the next
        // one.
        let prefixed = std::mem::take(&mut self.pending_prefix);
        // `^X` ALWAYS ARMS, including on top of an already-armed prefix: a human who presses it twice is
        // reaching for a chord, not asking for whatever `^X^X` might mean. Checked BEFORE the chord
        // below, because the other order made `^X` then `^X^R` report an unknown `Chord('x')` and then
        // spend the `^R` on the library's redo — a rescind that silently did nothing, which is what the
        // real-terminal gallery caught and no unit test had thought to ask.
        if ctrl && matches!(code, KeyCode::Char('x') | KeyCode::Char('X')) {
            self.pending_prefix = true;
            return Edit::Consumed;
        }
        if prefixed
            && ctrl
            && let KeyCode::Char(c) = code
            && c.is_ascii_alphabetic()
        {
            let c = c.to_ascii_lowercase();
            return if c == 'e' {
                Edit::Editor
            } else {
                Edit::Chord(c)
            };
        }

        match code {
            // A NEWLINE needs a chord, because Enter sends. All three forms, per the module docs.
            KeyCode::Char('j') if ctrl => {
                self.area.insert_newline();
                Edit::Consumed
            }
            KeyCode::Enter if ctrl || alt || shift => {
                self.area.insert_newline();
                Edit::Consumed
            }
            KeyCode::Enter => Edit::Send,
            KeyCode::Esc => Edit::Cancel,
            // The library's map does the rest: word motion (`Alt+b`/`Alt+f`), the kills (`Ctrl+W`,
            // `Alt+Backspace`, `Alt+d`, `Ctrl+K`), the line ends (`Ctrl+A`/`Ctrl+E`), undo and redo,
            // and plain typing.
            _ => {
                // The library's `input` returns whether the TEXT CHANGED, not whether the key was
                // handled — a cursor motion returns `false`. Either way the composer owns the keyboard
                // while it is open (nothing else is bound in this mode), so the key is consumed.
                self.area.input(Input::from(KeyEvent::new(code, mods)));
                Edit::Consumed
            }
        }
    }
}
