//! The adaptive bottom bar. It is adaptive twice over: by CONTEXT, offering only the
//! [`BINDINGS`] rows that apply to the selected row, and by WIDTH, shedding chips by
//! rank and then their labels until what is left fits. The fitting arithmetic lives
//! beside the bar it serves, because a chip whose measured width disagrees with its
//! drawn width is a bar that overflows.

use crate::*;

/// The keybar's chip separator, and the gap before the trailing status.
pub(crate) const KEYBAR_SEP: &str = " · ";

pub(crate) const KEYBAR_STATUS_GAP: &str = "   ";

/// Columns below which the trailing status is dropped rather than shown: a status
/// clipped to `"r…"` is noise, not feedback.
pub(crate) const KEYBAR_STATUS_MIN: usize = 6;

/// Keybar width tiers, in terminal COLUMNS.
///
/// At/above [`KEYBAR_LABELS_W`] chips carry `key + label` (today's bar). From
/// [`KEYBAR_KEYS_W`] up to it they are KEY-ONLY badges, which is what buys the room to
/// keep every applicable key on screen in a tmux split. Below [`KEYBAR_KEYS_W`] the bar
/// shrinks to the honest minimum — the `?` help hint and `q` — because a clipped bar
/// that silently hides its right-hand half is worse than a short one that admits it.
pub(crate) const KEYBAR_LABELS_W: u16 = 100;

pub(crate) const KEYBAR_KEYS_W: u16 = 50;

/// One resolved keybar chip: a [`BINDINGS`] row that applies right now, with its
/// context-sensitive label filled in.
pub(crate) struct Chip {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) rank: u8,
    pub(crate) click: Option<(KeyCode, KeyModifiers)>,
}

fn click_binding(key: &str) -> Option<(KeyCode, KeyModifiers)> {
    let plain = |code| Some((code, KeyModifiers::NONE));
    match key {
        "Enter" | "y/enter" => plain(KeyCode::Enter),
        "Esc" | "Esc/q" => plain(KeyCode::Esc),
        "Tab" => plain(KeyCode::Tab),
        "End" => plain(KeyCode::End),
        "^E" => Some((KeyCode::Char('e'), KeyModifiers::CONTROL)),
        "^X" => Some((KeyCode::Char('x'), KeyModifiers::CONTROL)),
        "?" => plain(KeyCode::Char('?')),
        key if key.len() == 1 => plain(KeyCode::Char(key.chars().next()?)),
        _ => None,
    }
}

/// Columns a chip occupies: [`keychip`] renders `" {key} "` plus `" {label}"` when
/// labelled.
pub(crate) fn chip_cols(c: &Chip, labels: bool) -> usize {
    text_cols(c.key) + 2 + if labels { text_cols(c.label) + 1 } else { 0 }
}

/// Columns the whole bar occupies: a leading space, the chips, and the separators
/// between them.
pub(crate) fn chips_cols(chips: &[Chip], labels: bool) -> usize {
    1 + text_cols(KEYBAR_SEP) * chips.len().saturating_sub(1)
        + chips.iter().map(|c| chip_cols(c, labels)).sum::<usize>()
}

/// Shed the least important chips — highest `rank` first, right-most on a tie — until
/// the bar fits `budget` columns. DISPLAY order is untouched.
///
/// Rank 0 (`?` and `q`) is shed LAST, so the way to see the full key list and the way
/// out survive wherever they can: at pmtui's [`MIN_W`] of 24 columns `? Help · q Quit`
/// still fits. Below that the bar is allowed to empty out rather than overflow — a
/// 4-column bar has nothing honest to say anyway.
pub(crate) fn fit_chips(mut chips: Vec<Chip>, labels: bool, budget: usize) -> Vec<Chip> {
    while chips_cols(&chips, labels) > budget {
        let Some(i) = chips
            .iter()
            .enumerate()
            .max_by_key(|(i, c)| (c.rank, *i))
            .map(|(i, _)| i)
        else {
            break; // nothing left to shed
        };
        chips.remove(i);
    }
    chips
}

/// The context-sensitive chip label for a binding.
///
/// `m` is a 2-value TOGGLE, so its chip names what the press will DO to the SELECTED row
/// rather than the dial's name: a bare "Mode" reads as "press m for autopilot ON", which is
/// actively misleading on a row that is already on Autopilot. The ARROW is what carries the
/// direction — `Mode \u{2192} standard` is a press with a destination, not a state readout,
/// which is the whole distinction the old "Autopilot off" label had to spell out in words.
/// With no readable tier we can't know the direction, so fall back to the bare noun.
///
/// `Enter` gets the same treatment for the same reason: on a PAUSED row it resumes rather
/// than attaching, and a bar reading `Enter Attach` in front of a row whose agent this UI
/// just killed describes something that cannot happen. Both `p` and `Enter` then name the
/// same one-key undo, which is the pair the human is looking for.
///
/// Every other binding uses the table's label verbatim.
pub(crate) fn chip_label(b: &Binding, sel: Option<&ProjectView>) -> &'static str {
    if b.scope != Scope::Normal {
        return b.label;
    }
    match b.key {
        "Enter" if sel.is_some_and(|v| !v.enabled) => "Resume",
        "s" if sel.is_some_and(|v| message_route(v) == MessageRoute::Answer) => "Answer",
        "m" => match sel.and_then(|v| v.tier) {
            Some(Tier::Autopilot) => "Mode \u{2192} standard",
            Some(Tier::Standard) => "Mode \u{2192} autopilot",
            None => "Mode",
        },
        _ => b.label,
    }
}

/// The [`Scope::Normal`] chips that APPLY to the current selection — see
/// [`Applies`]. Everything filtered out here stays discoverable through `?`.
fn binding_applies(app: &App, binding: &Binding) -> bool {
    let sel = app.selected_view();
    match binding.applies {
        Applies::Always => true,
        Applies::AnyRow => !app.projects.is_empty(),
        Applies::NotNative => sel.is_some(),
        Applies::Driven => sel.is_some_and(|v| pmd_drives(v.mode)),
        Applies::AgentLoop => sel.is_some(),
        Applies::Autopilot => {
            sel.is_some_and(|v| agent_manager::daemon::pmd_drives_row(v.mode, v.tier))
        }
        Applies::JobCommit => sel.is_some_and(|v| v.job && v.job_commit.is_some()),
        Applies::ReachesAgent => sel.is_some_and(|v| {
            matches!(
                message_route(v),
                MessageRoute::Message | MessageRoute::Answer
            )
        }),
    }
}

/// The keys that start, pause or branch the selected row in Normal mode and on the Task Board. A
/// row a spawn request staged, or a failed fork left behind, refuses every one of them
/// (`start_refusal`, read through [`start_refused`]), so no chip offers them there; `d`, `R` and
/// `w` still act.
fn starts_the_row(key: &str) -> bool {
    matches!(key, "Enter" | "m" | "p" | "r" | "f")
}

/// The [`Scope::Normal`] chips that APPLY to the current selection — see
/// [`Applies`]. Everything filtered out here stays discoverable through `?`.
pub(crate) fn normal_chips(app: &App) -> Vec<Chip> {
    let sel = app.selected_view();
    let unstartable = sel.is_some_and(start_refused);
    BINDINGS
        .iter()
        .filter(|b| b.scope == Scope::Normal)
        .filter(|b| binding_applies(app, b))
        .filter(|b| !(unstartable && starts_the_row(b.key)))
        .map(|b| Chip {
            key: b.key,
            label: chip_label(b, sel),
            rank: b.rank,
            click: click_binding(b.key),
        })
        .collect()
}

/// The chips for a non-Normal mode: every [`BINDINGS`] row in that mode's scope.
///
/// `confirming` is the pending [`Confirmable`] when the mode is [`UiMode::Confirming`],
/// because ONE overlay serves two different destructive actions and its `y` chip has to
/// name the one actually in front of the human. Caught on a real render: the restart
/// confirm first drew with a bar reading `y Remove`, which is the worst possible lie for
/// this overlay — the human is one keystroke from an irreversible action and the bar names
/// the wrong one. `chip_label` does the same job for `m` in Normal mode.
pub(crate) fn scope_chips(
    app: &App,
    scope: Scope,
    confirming: Option<Confirmable>,
    goal_turns_on_autopilot: bool,
    create_uses_message: bool,
) -> Vec<Chip> {
    BINDINGS
        .iter()
        .filter(|b| b.scope == scope && !b.label.is_empty())
        .filter(|b| scope != Scope::Board || binding_applies(app, b))
        .map(|b| Chip {
            key: b.key,
            label: match (b.key, confirming) {
                ("y", Some(Confirmable::Restart)) => "Restart",
                ("y", Some(Confirmable::Remove)) => "Delete",
                // The goal field reached from `m` does something `Save` does not describe: it
                // hands pmd the wheel. Same rule as the `y` chips above and `chip_label`'s `m` —
                // the bar names what the press will DO, and this is the one press in the UI where
                // getting that wrong starts an autonomous drive the human did not ask for.
                // Both prompts in `m`'s chain relabel the same two keys, so the condition is the
                // SCOPE-agnostic "this field turns autopilot on" rather than one arm per scope.
                ("Enter", _) if goal_turns_on_autopilot => "Autopilot ON",
                ("Esc", _) if goal_turns_on_autopilot => "Stay Standard",
                ("^E", _) if create_uses_message => "Edit message",
                _ => b.label,
            },
            rank: b.rank,
            click: click_binding(b.key),
        })
        .collect()
}

struct KeybarLayout {
    chips: Vec<Chip>,
    labels: bool,
    full: usize,
    budget: usize,
}

fn keybar_layout(app: &App, width: u16) -> Option<KeybarLayout> {
    if width == 0 {
        return None;
    }
    let full = usize::from(width).saturating_sub(1);
    let want = if app.status.is_empty() {
        0
    } else {
        text_cols(&app.status) + text_cols(KEYBAR_STATUS_GAP)
    };
    // HALF the bar, not a third. The third was set when a ` STATUS ` pane below the list could show
    // the line in full and wrap it; that pane is gone (the log is a file now), so this row is the only
    // place a status is ever read and it gets the larger share. The chips still win the remainder,
    // because a truncated status is recoverable — the whole line is in the log file — while a missing
    // `?` is not.
    let reserved = want.min(full / 2);
    let budget = full.saturating_sub(reserved);
    let (chips, labels) = match &app.mode {
        UiMode::Normal => {
            let all = normal_chips(app);
            if width >= KEYBAR_LABELS_W {
                (fit_chips(all, true, budget), true)
            } else if width >= KEYBAR_KEYS_W {
                // Key-only buys room for a full bar; a sparse one (an empty list's `? Help` and
                // `q Quit`) keeps its labels, so the help chip still reads as help.
                if chips_cols(&all, true) <= budget {
                    (all, true)
                } else {
                    (fit_chips(all, false, budget), false)
                }
            } else {
                let min: Vec<Chip> = all.into_iter().filter(|c| c.rank == 0).collect();
                if chips_cols(&min, true) <= budget {
                    (min, true)
                } else {
                    (fit_chips(min, false, budget), false)
                }
            }
        }
        other => {
            let scope = match other {
                UiMode::Answering { .. } => Scope::Answer,
                UiMode::Creating(_) => Scope::Create,
                UiMode::ConfirmCreateDir { .. } => Scope::ConfirmCreateDir,
                UiMode::EditingGoal { .. } => Scope::Goal,
                UiMode::EditingDirective { .. } => Scope::Directive,
                UiMode::EditingCadence { .. } => Scope::Cadence,
                UiMode::Sending { .. } => Scope::Send,
                UiMode::Renaming { .. } => Scope::Rename,
                UiMode::Switching { .. } => Scope::Switcher,
                UiMode::Board => Scope::Board,
                // An open dropdown has its own keys, and the keybar says which: a row's `Enter`
                // OPENS, a list's `Enter` SELECTS, and `Esc` there closes the list rather than the
                // view.
                UiMode::Settings { open: Some(_), .. } => Scope::SettingsPick,
                UiMode::Settings { .. } => Scope::Settings,
                UiMode::ModelPicker { .. } => Scope::ModelPicker,
                UiMode::Confirming { .. } => Scope::Confirm,
                UiMode::WakeView { .. } | UiMode::Decisions { .. } => Scope::Wake,
                UiMode::Help { .. } => Scope::Help,
                UiMode::Normal => unreachable!("Normal is handled above"),
            };
            let mut chips = scope_chips(
                app,
                scope,
                match &app.mode {
                    UiMode::Confirming { what, .. } => Some(*what),
                    _ => None,
                },
                matches!(
                    app.mode,
                    UiMode::EditingGoal {
                        then_autopilot: true,
                        ..
                    } | UiMode::EditingCadence {
                        then_autopilot: true,
                        ..
                    }
                ),
                matches!(
                    app.mode,
                    UiMode::Creating(ref form) if form.tier == Tier::Standard
                ),
            );
            if scope == Scope::Board {
                let paused = app.selected_view().is_some_and(|view| !view.enabled);
                // The same route `s` takes (`message_route`), so the chip names what the press does.
                let answer_required = app
                    .selected_view()
                    .is_some_and(|view| message_route(view) == MessageRoute::Answer);
                // The lane's `Enter` only opens the detail, so it survives on such a row; every
                // chip that starts it goes, as in Normal mode.
                let unstartable = app.selected_view().is_some_and(start_refused);
                chips.retain(|chip| {
                    !(unstartable
                        && starts_the_row(chip.key)
                        && (app.board_detail_open || chip.key != "Enter"))
                });
                if !app.board_detail_open {
                    let sel = app.selected_view();
                    let column = sel.map(board_column);
                    // A lane chip is published exactly where its Board key acts: `s` by the key
                    // guard's own predicate, `f` by `fork_selected`'s own refusal rule.
                    let message = sel.is_some_and(board_lane_message_applies);
                    let fork = sel.is_some_and(|view| fork_refusal(view).is_none());
                    // The Board binds no `?`, so where the top controls yield `Esc` stays as the
                    // way back to the Session view, whose keybar keeps Help.
                    let back = !top_controls_shown(app, width);
                    chips.retain(|chip| match chip.key {
                        "Enter" | "r" | "d" | "R" => true,
                        "Esc" => back,
                        "s" => message,
                        "f" => fork,
                        "p" => matches!(
                            column,
                            Some(
                                BoardColumn::Paused | BoardColumn::Autopilot | BoardColumn::Working
                            )
                        ),
                        "v" => column == Some(BoardColumn::Autopilot),
                        _ => false,
                    });
                    for chip in &mut chips {
                        if chip.key == "p" && paused {
                            chip.label = "Resume";
                        } else if chip.key == "s" && answer_required {
                            chip.label = "Answer";
                        }
                    }
                } else {
                    chips.retain(|chip| matches!(chip.key, "Enter" | "R" | "s" | "m" | "Esc"));
                    for chip in &mut chips {
                        chip.label = match chip.key {
                            "Enter" if paused => "Resume",
                            "Enter" => "Attach",
                            "s" if answer_required => "Answer",
                            "Esc" => "Back",
                            _ => chip.label,
                        };
                    }
                }
            }
            if chips_cols(&chips, true) <= budget {
                (chips, true)
            } else {
                (fit_chips(chips, false, budget), false)
            }
        }
    };
    Some(KeybarLayout {
        chips,
        labels,
        full,
        budget,
    })
}

/// The bottom keybar as one clipped-proof [`Line`], so a test can measure it.
///
/// ADAPTIVE in two independent ways. By WIDTH: the three tiers around
/// [`KEYBAR_LABELS_W`]/[`KEYBAR_KEYS_W`], plus a final [`fit_chips`] pass that
/// guarantees the line fits `width` whatever the tier decided. By CONTEXT: in Normal
/// mode only the actions that apply to the selected row are offered (see [`Applies`]).
/// Anything dropped is still listed by the `?` overlay — that is what makes dropping
/// honest rather than lossy.
/// (see `render`), which also hands the chips back the third of the bar the status used to reserve.
pub(crate) fn keybar_line(app: &App, width: u16) -> Line<'static> {
    let Some(KeybarLayout {
        chips,
        labels,
        full,
        budget,
    }) = keybar_layout(app, width)
    else {
        return Line::default();
    };

    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, c) in chips.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(KEYBAR_SEP, dim));
        }
        spans.extend(keychip(c.key, if labels { c.label } else { "" }));
    }
    let mut used = chips_cols(&chips, labels);
    if matches!(app.mode, UiMode::Confirming { .. }) {
        // The confirm overlay PROMISES this, so spend the columns on it before the
        // status — but only if they exist.
        let note = "   any other key cancels";
        if used + text_cols(note) <= budget {
            used += text_cols(note);
            spans.push(Span::styled(note, dim));
        }
    }
    // The status trails the chips, taking its reservation plus whatever the chips left
    // over, and is truncated into that. Too few columns to read and it is dropped
    // entirely — the bar stays honest either way, and `?` is always still on it.
    if !app.status.is_empty() {
        let room = full
            .saturating_sub(used)
            .saturating_sub(text_cols(KEYBAR_STATUS_GAP));
        if room >= KEYBAR_STATUS_MIN {
            spans.push(Span::raw(KEYBAR_STATUS_GAP));
            // A FAILURE IS NOT GREEN. Every status was drawn green, which painted
            // "send failed: no such session" in the colour of success — and `s` reports
            // the driver's error verbatim, so this is the first status that routinely
            // says something went wrong.
            let failed = status_is_failure(&app.status);
            spans.push(Span::styled(
                truncate(&app.status, room),
                if failed {
                    Style::default()
                        .fg(agent_manager::theme::hard())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(agent_manager::theme::live())
                },
            ));
        }
    }
    Line::from(spans)
}

/// Bottom keybar: adaptive per [`UiMode`] and per terminal width (see
/// [`keybar_line`]), rendered as reversed key chips with dim labels, plus the
/// transient `app.status` (green) when it fits.
pub(crate) fn render_keybar(f: &mut Frame, app: &App, area: Rect) {
    // The bar is the final layer. Clear its entire row first so a compact modal's
    // border cannot remain visible between or after the active chips — then paint the theme's own pair
    // back, because `Clear` leaves the TERMINAL's default one and this row is mostly gaps between
    // chips. Without it the keybar was a stripe of the terminal's background across a themed screen.
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(attention::canvas()), area);
    f.render_widget(Paragraph::new(keybar_line(app, area.width)), area);
    let mut hits = Vec::new();
    if let Some(layout) = keybar_layout(app, area.width) {
        let mut x = area.x.saturating_add(1);
        for (index, chip) in layout.chips.iter().enumerate() {
            if index > 0 {
                x = x.saturating_add(u16::try_from(text_cols(KEYBAR_SEP)).unwrap_or(u16::MAX));
            }
            let width = u16::try_from(chip_cols(chip, layout.labels)).unwrap_or(u16::MAX);
            if let Some((code, modifiers)) = chip.click {
                hits.push(KeyHit {
                    area: Rect::new(x, area.y, width, area.height.min(1)),
                    code,
                    modifiers,
                });
            }
            x = x.saturating_add(width);
        }
    }
    *app.key_hits.borrow_mut() = hits;
}
