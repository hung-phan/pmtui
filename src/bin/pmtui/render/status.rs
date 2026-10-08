//! The one-line top bar: the brand, the tallies over every row, and the daemon
//! chip. The tallies are counted here rather than cached on `App`, so the number on
//! screen is always over the rows on screen.

use crate::*;

/// The (needs-you, running, idle, stuck) tallies for the header, using the same
/// bucketing as the list glyphs so counts and rows always agree. Every row lands in
/// exactly one bucket, so the four always sum to `app.projects.len()`.
pub(crate) fn counts(app: &App) -> (usize, usize, usize, usize) {
    let (mut needs, mut running, mut idle, mut stuck) = (0, 0, 0, 0);
    for v in &app.projects {
        match status_category(v) {
            0 => needs += 1,
            1 => running += 1,
            3 => stuck += 1,
            _ => idle += 1,
        }
    }
    (needs, running, idle, stuck)
}

/// The bar's lead through the daemon chip: the brand, the VIEW TABS, the need-you call to action,
/// and daemon health. The top controls may never clip it.
fn lead_spans(app: &App, width: u16) -> Vec<Span<'static>> {
    let (needs, _, _, stuck) = counts(app);
    let (ng, _nc) = category_glyph(0); // glyph feeds the CTA chip; the need-you tally is gone (see below)
    let (sg, _sc) = category_glyph(3);
    let mut spans = vec![
        // Was `fg(White).bg(Blue)` — measured 1.06:1 on Catppuccin Mocha, 1.56 on
        // TokyoNight, 2.34 on Nord. The only background in the whole binary was spent on
        // a brand label, and on most dark themes it was invisible. Same chip SHAPE,
        // reached through reverse video (see `attention::chip_brand`).
        Span::styled(" pmtui ", attention::chip_brand()),
        Span::raw("  "),
    ];
    spans.extend(tab_spans(app, width));
    // THE CALL TO ACTION, in the loudest pixels on the screen, BEFORE the tally.
    //
    // This line is a clipped `Paragraph` with no wrap, so what comes first survives a
    // narrow terminal — and until now the whole bar could say nothing louder than a
    // dim-ish `◐ 2 need-you` while `escalation::Escalation::for_stops` was already
    // firing a desktop notification reading "2 decisions need you". pmtui was quieter
    // than `notify-send`.
    //
    // Absent entirely at zero, the same "zero is not news" rule the stuck count follows.
    let wants_you = needs + stuck;
    if wants_you > 0 {
        let any_hard = app
            .projects
            .iter()
            .any(|v| attention::level(v) == attention::Level::Hard);
        // SHAPE FOLLOWS THE BUCKET, HUE FOLLOWS SEVERITY — the same rule the rows use,
        // and the reason this is split rather than one condition. Choosing `✕` because a
        // STOP is Hard borrowed the stuck bucket's glyph for a row drawing `◐`, which is
        // the very glyph/severity disagreement this milestone set out to fix — caught by
        // looking at a real screen, not by any test.
        let chip_glyph = if stuck > 0 { sg } else { ng };
        let chip_style = if any_hard {
            attention::fill_hard()
        } else {
            attention::fill_soft()
        };
        // THE CHIP SHEDS ITS WORDS BEFORE THE BAR DAMAGES THE DAEMON FACT. `need you` costs nine
        // columns, and the chip sits between the tab strip and `pmd DOWN`, so on a ~50-column
        // terminal the third tab was enough to clip the daemon chip to `pmd DO` — the ambiguous
        // fragment this lead exists to prevent. Glyph, count and the loud fill stay: they ARE the
        // call to action, the words only name it. Same shed order as the tabs and the top controls,
        // which drop labels before badges.
        //
        // Measured against the columns already spent rather than a width constant, because the
        // count's own width varies. The tabs come BEFORE this, so no hit region moves with the fleet.
        let spent: usize = spans.iter().map(Span::width).sum();
        let (daemon, _) = app.daemon_live().chip();
        let words = format!(" {chip_glyph} {wants_you} need you ");
        let chip = if spent + text_cols(&words) + 2 + text_cols(daemon) <= usize::from(width) {
            words
        } else {
            format!(" {chip_glyph} {wants_you} ")
        };
        spans.push(Span::styled(chip, chip_style));
        spans.push(Span::raw("  "));
    }
    // Daemon health precedes routine tallies so the persistent top controls can
    // never clip `pmd DOWN` into an ambiguous fragment on laptop-width screens.
    let (pmd_text, pmd_color) = app.daemon_live().chip();
    let pmd_style = if pmd_color == agent_manager::theme::hard() {
        attention::fill_hard()
    } else {
        Style::default().fg(pmd_color).add_modifier(Modifier::BOLD)
    };
    spans.push(Span::styled(pmd_text, pmd_style));
    spans
}

/// Top status bar: the ` pmtui ` chip + colorized running/need-you/stuck/idle counts
/// (each carrying its bucket's glyph as well as its colour, per [`category_glyph`]) +
/// the `pmd up`/`pmd DOWN` indicator + a dim socket hint.
///
/// The daemon indicator comes BEFORE the socket on purpose. `app.socket` is a tmux
/// SOCKET NAME, not liveness — it reads identically whether or not anything is running,
/// and its presence was part of why nothing on this screen ever answered "is a daemon
/// running?". This line is a plain `Paragraph` (clipped at the right edge, never
/// wrapped), so putting the answer first means a narrow terminal loses the socket hint
/// rather than the fact that matters.
///
/// Cost: `app.daemon_live()` is CACHED ([`DAEMON_PROBE_TTL`]) — the bar is redrawn every
/// ~500ms tick and must not `open()` the lock file per frame.
pub(crate) fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    // One column clear on each side ([`BAR_PAD_X`]), so the brand chip's text lands in the same
    // column as the pane titles below it instead of overhanging the frame's corner. Everything
    // downstream — the width tiers, the controls' right edge, every hit region — is derived from
    // this rect, so the tab a click lands on is the tab that was drawn.
    let area = bar_inset(area);
    let (_, running, idle, stuck) = counts(app);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let (rg, rc) = category_glyph(1);
    let (ig, ic) = category_glyph(2);
    let (sg, sc) = category_glyph(3);
    let mut spans = lead_spans(app, area.width);
    // Columns through the daemon chip: the part of this line the top controls may never clip.
    let daemon_end: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::styled("  · ", dim));
    // NO SEPARATE need-you TALLY. The loud CTA chip above already carries the need-you count (it
    // is `needs + stuck`), so a dim `◐ N need-you` here just said the same thing a second time —
    // user: *"i see 2 need you … '1 need you', '0 running', 'icon 1 need-you'. This is redundant"*.
    // Running stays the tally's anchor; stuck (a distinct word, shown only when non-zero) and idle
    // follow.
    spans.push(Span::styled(
        format!("{rg} {running} running"),
        Style::default().fg(rc),
    ));
    // The stuck bucket is shown ONLY when it is non-zero, and next to need-you because
    // both are the human's to act on. This line is a clipped `Paragraph` and the
    // `pmd up`/`pmd DOWN` chip sits to its right, so a permanently-present fourth count
    // would spend a narrow terminal's last columns reporting a zero and push the daemon
    // fact — the one thing nothing else on this screen says — off the edge.
    if stuck > 0 {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("{sg} {stuck} stuck"),
            Style::default().fg(sc).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(
        format!("{ig} {idle} idle"),
        Style::default().fg(ic),
    ));
    if !app.socket.is_empty() {
        // Labelled `sock`, because the default socket is itself NAMED "pmd": unlabelled
        // it rendered as "· pmd up  · pmd", which reads as the daemon being mentioned
        // twice rather than as a daemon state plus the tmux socket it drives.
        spans.push(Span::styled(format!("  · sock {}", app.socket), dim));
    }
    // The tabs are drawn as part of the lead, so their hit regions are published even where the
    // action controls yield — they are location, and they survive longest.
    let mut hits = tab_hits(app, area);
    let Some(controls) = top_controls(app, area.width, daemon_end) else {
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        *app.top_hits.borrow_mut() = hits;
        return;
    };
    let key_style = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
    let action_cols = controls_cols(&controls);
    let action_width = u16::try_from(action_cols)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [main, controls_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(action_width)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)), main);
    let mut action_spans = vec![Span::styled(" │ ", dim)];
    let mut x = controls_area.x.saturating_add(3);
    for (index, (key, label, code)) in controls.iter().enumerate() {
        if index > 0 {
            action_spans.push(Span::styled(KEYBAR_SEP, dim));
            x = x.saturating_add(u16::try_from(text_cols(KEYBAR_SEP)).unwrap_or(u16::MAX));
        }
        action_spans.push(Span::styled(*key, key_style));
        if !label.is_empty() {
            action_spans.push(Span::styled(format!(" {label}"), dim));
        }
        let width = u16::try_from(control_cols(key, label)).unwrap_or(u16::MAX);
        hits.push(KeyHit {
            area: Rect::new(
                x,
                controls_area.y,
                width.min(controls_area.right().saturating_sub(x)),
                1,
            ),
            code: *code,
            modifiers: KeyModifiers::NONE,
        });
        x = x.saturating_add(width);
    }
    f.render_widget(Paragraph::new(Line::from(action_spans)), controls_area);
    *app.top_hits.borrow_mut() = hits;
}

// ── The view tabs ────────────────────────────────────────────────────────────────
//
// TABS, not chips. The first shot at this put `1 Sessions` and `2 Tasks` in the top-right control
// row, where they sat among `/ Switch` and `n New` as four identical badges — one row that mixed
// WHERE YOU ARE with WHAT YOU CAN DO, so finding your location meant reading it. User: *"The
// indicator / Switch · 1 Sessions · 2 Tasks · n New, is not a good UI design, you may need to have
// Sth like Tabs, so people don't need to think"*.
//
// So the two views LEAD the bar, right after the brand chip, and the active one is UNDERLINED as
// well as bold and cyan. The underline is the terminal attribute, never a drawn `─` row: a tab strip
// that cost a second row would take it from the session list, and this dashboard's scarcest resource
// is rows.

/// Columns the brand chip and its gap occupy, i.e. the first tab's x offset inside the bar.
///
/// The tabs sit BEFORE the need-you chip so this offset is a constant: a chip that appears the
/// moment a session blocks must not slide a click target out from under the pointer.
const TAB_X: u16 = 9;

/// Tab labels from this width. Below it the tabs keep their `1`/`2`/`0` badges — narrower is exactly
/// when knowing which view you are in matters most, and a badge still carries the underline.
///
/// Raised with the third tab: ` Settings ` plus its badge is eleven more columns, and at the old
/// threshold the labelled strip pushed the daemon chip off the end.
const TAB_LABELS_W: u16 = 84;

/// Below this the tabs yield: even the bare badges and their rule would push `pmd DOWN` off the end,
/// and an ambiguous daemon fragment is worse than no tab strip.
///
/// Raised with the third tab, by the same derivation that set the old floor of 28: the brand chip and
/// its gap (9) plus three badges, two gaps and the rule (16) plus a whole `pmd DOWN` (8) is 33. One
/// column less and the daemon fact is a fragment.
const TAB_MIN_W: u16 = 33;

/// The dim vertical rule that separates the tabs from the state readout.
const TAB_RULE: &str = " │ ";

/// One view tab: its key badge, its label (empty on the badge-only tier), the key it selects the
/// view with, and whether it is the view on screen.
type ViewTab = (&'static str, &'static str, KeyCode, bool);

/// How many view tabs there are: `1 Sessions`, `2 Tasks`, `0 Settings`.
const TABS: usize = 3;

/// The style that says WHERE YOU ARE: bold, cyan and underlined.
fn active_tab_style() -> Style {
    Style::default()
        .fg(agent_manager::theme::accent())
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// The two view tabs at `width`, or `None` outside the two projections (an overlay's `1`/`2` type
/// into its field, so publishing a clickable tab there would not route to the same key) or where
/// they yield.
fn view_tabs(app: &App, width: u16) -> Option<[ViewTab; TABS]> {
    // `0 Settings` sits AFTER the two working views and is numbered last on purpose: the views a human
    // spends the day in keep `1` and `2`, and a preference is somewhere you visit.
    let here = match &app.mode {
        UiMode::Normal => 0,
        UiMode::Board => 1,
        UiMode::Settings { .. } => 2,
        _ => return None,
    };
    if width < TAB_MIN_W {
        return None;
    }
    let labelled = width >= TAB_LABELS_W;
    Some(
        [
            (" 1 ", "Sessions", KeyCode::Char('1')),
            (" 2 ", "Tasks", KeyCode::Char('2')),
            (" 0 ", "Settings", KeyCode::Char('0')),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (key, label, code))| {
            (key, if labelled { label } else { "" }, code, index == here)
        })
        .collect::<Vec<_>>()
        .try_into()
        .expect("one tab per view"),
    )
}

/// The tab strip's spans, including its trailing rule. Empty where the tabs yield, so the lead then
/// reads exactly as it did before they existed.
fn tab_spans(app: &App, width: u16) -> Vec<Span<'static>> {
    let Some(tabs) = view_tabs(app, width) else {
        return Vec::new();
    };
    let mut spans = Vec::with_capacity(6);
    for (index, (key, label, _, active)) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        // The badge keeps the keybar's reversed-bold style. It carries the underline ONLY on the
        // badge-only tier, where there is no label to carry it — the location cue must not be the
        // thing that narrowness removes.
        let mut badge = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        if *active && label.is_empty() {
            badge = badge.add_modifier(Modifier::UNDERLINED);
        }
        spans.push(Span::styled(*key, badge));
        if !label.is_empty() {
            spans.push(Span::styled(
                format!(" {label}"),
                if *active {
                    active_tab_style()
                } else {
                    Style::default().add_modifier(Modifier::DIM)
                },
            ));
        }
    }
    spans.push(Span::styled(
        TAB_RULE,
        Style::default().add_modifier(Modifier::DIM),
    ));
    spans
}

/// Where each tab was drawn, so a click routes through the same `handle_key` its badge advertises.
/// Empty where the tabs yield, and clamped to `area`, because a hit region over text the bar never
/// drew is a lie.
fn tab_hits(app: &App, area: Rect) -> Vec<KeyHit> {
    let Some(tabs) = view_tabs(app, area.width) else {
        return Vec::new();
    };
    let mut hits = Vec::with_capacity(tabs.len());
    let mut x = area.x.saturating_add(TAB_X);
    for (index, (key, label, code, _)) in tabs.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(2);
        }
        let width = u16::try_from(control_cols(key, label)).unwrap_or(u16::MAX);
        hits.push(KeyHit {
            // Clamped to the bar, exactly as the top-right controls are: a click can never land on
            // a column the text did not reach, and `rect_contains` ignores a zero-width region.
            area: Rect::new(x, area.y, width.min(area.right().saturating_sub(x)), 1),
            code: *code,
            modifiers: KeyModifiers::NONE,
        });
        x = x.saturating_add(width);
    }
    hits
}

/// One top-right control: its key badge, its label (empty when key-only), and the key it sends.
type TopControl = (&'static str, &'static str, KeyCode);

/// The `/` and `n` controls the status bar draws at `width` beside a lead `daemon_end` columns wide,
/// or `None` when this mode has none or they yield.
///
/// These are ACTIONS only; the two views are tabs in the lead now (see above). Labelled from 72
/// columns and only while the labels still leave the daemon chip whole — the lead includes the tab
/// strip, so this check is what keeps a wide tab strip plus a loud need-you chip from turning
/// `pmd DOWN` into an ambiguous fragment. Otherwise both drop to key-only badges (as the keybar's
/// own key tier does) so Switch and New stay visible and clickable; only when even those do not fit
/// do they yield together.
fn top_controls(app: &App, width: u16, daemon_end: usize) -> Option<[TopControl; 2]> {
    let new_label = match &app.mode {
        UiMode::Normal => "New",
        UiMode::Board => "+ Task",
        _ => return None,
    };
    let tier = |labelled: bool| {
        [
            (" / ", "Switch", KeyCode::Char('/')),
            (" n ", new_label, KeyCode::Char('n')),
        ]
        .map(|(key, label, code)| (key, if labelled { label } else { "" }, code))
    };
    let fits =
        |controls: &[TopControl; 2]| daemon_end + controls_cols(controls) <= usize::from(width);
    let labelled = tier(true);
    if width >= 72 && fits(&labelled) {
        return Some(labelled);
    }
    let keys_only = tier(false);
    fits(&keys_only).then_some(keys_only)
}

/// Whether the status bar draws its top-right controls at `width`. The Task view binds no `?`,
/// so its keybar keeps the way back to the Session view where they yield.
pub(crate) fn top_controls_shown(app: &App, width: u16) -> bool {
    // `width` is the whole row, as the keybar sees it; the bar itself draws inside
    // [`bar_inset`]. Answering from the full width would let the keybar drop its way back at a
    // width where the controls had already yielded — two bars disagreeing by two columns.
    let width = bar_width(width);
    let daemon_end = lead_spans(app, width).iter().map(Span::width).sum();
    top_controls(app, width, daemon_end).is_some()
}

fn control_cols(key: &str, label: &str) -> usize {
    text_cols(key)
        + if label.is_empty() {
            0
        } else {
            1 + text_cols(label)
        }
}

/// Columns the controls take, including the ` │ ` rule that sets them apart.
fn controls_cols(controls: &[TopControl]) -> usize {
    3 + text_cols(KEYBAR_SEP) * controls.len().saturating_sub(1)
        + controls
            .iter()
            .map(|(key, label, _)| control_cols(key, label))
            .sum::<usize>()
}

/// The VERDICT line, drawn just above the body when any session needs you (the caller gates it on
/// `needs + stuck > 0`): WHICH sessions those are. The status bar's CTA chip carries the COUNT;
/// this carries the identities the count cannot — each session as `<glyph> <id>` in its severity
/// hue, HARD (stuck / a hard stop) first so the loudest sits leftmost, then by id for a stable
/// order. A clipped `Paragraph` like the status bar (never wrapped): it names as many as fit and
/// appends a dim `+N more` when the row runs out of room, reserving space so the last name never
/// crowds the tail out. It is the on-screen echo of "silence is the default state" — present
/// exactly when the fleet is not calm, and gone the instant it is.
pub(crate) fn render_verdict_line(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    // The needy rows, HARD first, then by id. `attention::level` is the same severity read the
    // status-bar CTA and the row fills use, so the leftmost name here is the reddest one there.
    // `v.enabled &&` mirrors the count's bucketing: `status_category` buckets a PAUSED row as idle
    // (early-return on `!enabled`), so it never enters `needs + stuck` and never shows in the CTA
    // chip. Without this guard a paused row whose ledger is still Blocked (posture NeedsYou/Stuck)
    // would be NAMED here — drawn with the idle `○` glyph, and disagreeing with the count above.
    let mut needy: Vec<&ProjectView> = app
        .projects
        .iter()
        .filter(|v| v.enabled && v.posture.needs_attention())
        .collect();
    needy.sort_by(|a, b| {
        let rank = |v: &ProjectView| {
            if attention::level(v) == attention::Level::Hard {
                0
            } else {
                1
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.id.cmp(&b.id))
    });

    let width = usize::from(area.width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for v in &needy {
        let (glyph, color) = status_glyph(v);
        // `  ✕ infra-migrate` — a two-space gap between entries; glyph + id in the row's own hue,
        // bold, so the verdict reads as the loud thing it is. The id is never truncated: a clipped
        // session name is worse than an honest `+N more`, so we stop admitting whole names instead.
        let chunk = format!("  {glyph} {}", v.label());
        let w = text_cols(&chunk);
        // Reserve ~` +N more` (9 cols) so the tail always fits once at least one name is shown.
        if shown > 0 && used + w > width.saturating_sub(9) {
            break;
        }
        spans.push(Span::styled(
            chunk,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        used += w;
        shown += 1;
    }
    let hidden = needy.len().saturating_sub(shown);
    if hidden > 0 {
        spans.push(Span::styled(format!("  +{hidden} more"), dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
