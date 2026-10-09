//! The PURE decisions behind Enter and the armed auto-open, each with the enum that
//! names its arms. They are here rather than inside `App` for one reason: every one of
//! them is a routing rule that must be unit-testable without a tty, a tmux server or a
//! flock, so `App` computes the observable facts and these functions choose.

use crate::*;

/// Whether an answer written for this row would actually REACH the agent — the `a`
/// gate (see [`App::begin_answer`]).
///
/// Delegates to the daemon's own predicate rather than re-deriving the rule, so pmtui
/// can never disagree with the sweep about who is driving a row. That matters because
/// the delivery chain is entirely pmd's: `answers.json` is only ever read by
/// `JobScheduler::on_blocked`, which only runs inside the tick
/// [`agent_manager::daemon::pmd_drives_row`] gates.
///
/// `mode` alone is not enough (an Autopilot agent-loop row IS answerable) and `tier`
/// alone is not either (a `Mode::Auto` row is driven at every tier and stays
/// answerable) — which is exactly the two-input shape the shared predicate has.
///
/// An UNREADABLE tier now closes this gate, because driving became opt-in: nothing is
/// driving such a row, so nothing would ever read the `answers.json` we wrote. Delegating
/// rather than re-deriving is what made that follow for free — pmtui cannot drift from the
/// sweep on the one question that decides whether an answer is deliverable at all.
pub(crate) fn answer_reaches_the_agent(mode: Mode, tier: Option<Tier>) -> bool {
    pmd_drives(mode) && agent_manager::daemon::pmd_drives_row(mode, tier)
}

/// Where `s` leads for a row. The key, its keybar chip, and the composer shelf all read this
/// one precondition, so neither surface can promise an action the key refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageRoute {
    /// No open stop: `s` opens the ordinary message composer.
    Message,
    /// An open stop pmd would deliver an answer for: `s` opens Answer.
    ///
    /// AN OPEN STOP IS ONLY AN ANSWER WHEN AN ANSWER CAN BE DELIVERED. A row nothing drives used to
    /// have a third variant here that refused `s` and pointed at `m`, and it was a dead end: the row
    /// said "needs you", and the one key for acting on it asked the human to change the session's
    /// autonomy first — so that an answer could go through a file only a driven row's tick reads.
    ///
    /// Such a row is an ORDINARY [`Self::Message`] row, with nothing special about it. The preview
    /// already hides its stop block on the user's own ruling — *"when autopilot is off, the answer
    /// panel still shows. This isn't correct, since user will drive, we don't need to do anything
    /// here"* — because the agent's question is on screen in its own terminal, which is where the
    /// human answers it, and `s` delivers exactly there. So there is no second kind of answering to
    /// name, and every surface that would have had to special-case one keeps one fewer branch.
    Answer,
    /// A row a failed fork left behind: it has no conversation to message, and any live
    /// terminal under it is the unverified child, so `s` refuses and names `d`.
    IncompleteFork,
    /// A row a spawn request staged and has not launched: its first Message belongs to that
    /// request, so `s` refuses (`start_refusal`) until the broker starts it.
    SpawnStaged,
}

pub(crate) fn message_route(v: &ProjectView) -> MessageRoute {
    // Staged first, as `start_refusal` checks it first: the row waits on its broker.
    if v.spawn_staged {
        MessageRoute::SpawnStaged
    } else if v.incomplete_fork {
        MessageRoute::IncompleteFork
    // A stop makes this an Answer only if an answer would REACH the agent; see `Answer`'s own note
    // for why an undriven row's stop leaves it an ordinary Message row.
    } else if !v.stops.is_empty() && answer_reaches_the_agent(v.mode, v.tier) {
        MessageRoute::Answer
    } else {
        MessageRoute::Message
    }
}

/// The daemon-owned worker pane to WATCH for an agent-loop session, from its
/// `driver.json` (if any): `Some(pane)` only when a wake is on record as
/// `Running` with a non-empty pane name. Pure and tmux-free — the caller still
/// probes tmux liveness before attaching, since the daemon may have already
/// reaped a wake that finished between the write and this read.
pub(crate) fn watch_pane(driver: Option<&DriverState>) -> Option<String> {
    match driver {
        Some(d) if d.exit_reason == ExitReason::Running && !d.pane.is_empty() => {
            Some(d.pane.clone())
        }
        _ => None,
    }
}

/// What Enter should do on an agent-loop row. PURE so the four branches are
/// unit-tested without tmux/tty — the impure `request_attach` computes the inputs
/// (a live-and-alive wake pane, the ledger's `conversation_id`, and whether its
/// `run` is `Running`) and dispatches on the result.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnterAction {
    /// A live wake is in flight → attach-to-WATCH its daemon-owned pane (read-only).
    Watch(String),
    /// Parked with a captured conversation → drop into a real interactive REPL on it.
    Chat(String),
    /// No conversation yet → it is minted/captured on the first heartbeat; wait.
    WaitingFirstWake,
    /// A wake is `Running` but its pane wasn't found alive (a rare race) → nothing
    /// to watch or chat right now; show the cadence hint.
    NoWake,
}

/// Decide Enter's fallback after the unified project terminal was confirmed absent.
/// A live recorded wake pane is watched; otherwise a parked conversation may be
/// relaunched, a never-started conversation waits for its first wake, and a stale
/// Running record with no pane reports NoWake.
pub(crate) fn agent_loop_enter(
    live_pane: Option<&str>,
    conversation_id: Option<&str>,
    run_is_running: bool,
) -> EnterAction {
    if let Some(pane) = live_pane {
        return EnterAction::Watch(pane.to_string());
    }
    match conversation_id {
        Some(id) if !run_is_running => EnterAction::Chat(id.to_string()),
        None => EnterAction::WaitingFirstWake,
        _ => EnterAction::NoWake,
    }
}

/// The id pmtui keys create-vs-resume on: the ledger's `conversation_id` (the
/// daemon's sole authority) wins; the registry seed is the fallback used only when
/// the ledger has none yet (a session pmtui created-and-seeded but no wake has
/// adopted into the ledger). Pure — computed in `request_attach` and fed to
/// [`agent_loop_enter`], so a second Enter on a seed-only session yields
/// `Chat(seed)`, never a re-mint.
/// `captured_cid` is the id the ENGINE itself last reported — codex's `notify` hook writes its
/// `thread_id` to `ProjectPaths::codex_conversation_id`. It ranks LAST deliberately: the ledger and
/// the registry are DECISIONS (pmd's and pmtui's), while this is an observation, so putting it last
/// keeps the change purely additive — it can only decide a row where both others are empty. That is
/// exactly the codex case, which has no caller-chosen id to record in either. Before this, a codex
/// session that had run for hours still looked never-woken, so Enter opened a SECOND conversation
/// and the first became unreachable.
pub(crate) fn effective_id(
    ledger_cid: Option<&str>,
    registry_cid: Option<&str>,
    captured_cid: Option<&str>,
) -> Option<String> {
    ledger_cid
        .or(registry_cid)
        .or(captured_cid)
        .map(str::to_string)
}

/// The id codex's own turn hook recorded for this session, validated.
///
/// A thin alias over [`agent_manager::state::codex_identity`], which pmd reads too — ONE rule,
/// two readers, so the dashboard and the daemon can never disagree about which conversation a
/// codex terminal is on.
pub(crate) fn read_captured_conversation_id(paths: &ProjectPaths) -> Option<String> {
    agent_manager::state::codex_identity::read(paths)
}

/// What Enter does on a never-woken agent-loop row (`effective_id` is `None`),
/// keyed on the engine and whether the per-session `driver.lock` is FREE (pmd is
/// not driving). PURE so the route + honest human copy is unit-tested without a
/// real flock / tty; the impure lease acquire + mint + registry seed happen in
/// `request_attach`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FirstWakeAction {
    /// claude + free lock ⇒ CREATE-and-chat immediately (mint an id, seed the
    /// registry, hold the lease through the REPL). Carries no copy — the caller
    /// builds the create status once the mint/seed succeeds.
    CreateAndChat,
    /// pmd is driving (claude or codex + held lock) ⇒ ARM the auto-open and show
    /// this honest status meanwhile.
    Arm(String),
    /// codex + free lock (pmd is down) ⇒ open a FRESH codex chat NOW. Codex has no
    /// caller-chosen conversation id, so there is nothing to mint or seed and nothing
    /// to arm; the human is present, so pmtui launches a live interactive REPL and lets
    /// codex create the conversation itself. Carries the create status. The caller
    /// drops the lease first (Standard ⇒ pmd won't drive this row, so nothing races).
    CreateChatNoSeed(String),
}

/// Decide the never-woken Enter action + copy from `(engine, lease_free)`.
/// - claude + free lock → create-and-chat now.
/// - claude + held lock → arm ("creating on the first wake — you'll drop in
///   automatically").
/// - codex + held lock → arm ("codex: the conversation is created after the first
///   wake completes").
/// - codex + free lock → open a FRESH codex chat now (no id to mint/seed — codex
///   creates the conversation itself; a live REPL opens like claude's create-and-chat).
pub(crate) fn first_wake_action(engine: Engine, lease_free: bool, id: &str) -> FirstWakeAction {
    match (engine, lease_free) {
        (Engine::Claude, true) => FirstWakeAction::CreateAndChat,
        (Engine::Claude, false) => FirstWakeAction::Arm(format!(
            "{id}: creating on the first wake — you'll drop in automatically"
        )),
        (Engine::Codex, false) => FirstWakeAction::Arm(format!(
            "{id}: codex: the conversation is created after the first wake completes"
        )),
        (Engine::Codex, true) => FirstWakeAction::CreateChatNoSeed(format!(
            "creating a codex chat for {id} — a live REPL opens now (Ctrl+q returns; codex creates the conversation)"
        )),
    }
}

/// Which ROUTE Enter takes on a never-woken agent-loop row — decided by the TIER, and
/// decided BEFORE any `driver.lock` probe. See [`first_enter_route`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FirstEnterRoute {
    /// `Tier::Autopilot`: the human asked for a hands-off drive, so pmd is the intended
    /// writer of this conversation. Ensure a daemon and ARM the auto-attach; never mint
    /// an id, never seed the registry, never open a chat.
    EnsureDaemonThenArm,
    /// `Tier::Standard`: today's lease-keyed create-and-chat / arm / nudge routing,
    /// byte-for-byte. Chattable-on-create is the POINT of Standard — you can talk to a
    /// brand-new session immediately even with pmd down. Since m15 that is the WHOLE
    /// story for Standard: pmd does not drive such a row at all
    /// (`agent_manager::daemon::pmd_drives_row`), so this route is not a fallback while
    /// waiting for the daemon — it IS how a Standard session runs.
    LeaseKeyed,
}

/// Route the never-woken Enter on the AUTONOMY DIAL, not on a lease race.
///
/// This is the whole fix. `Tier` was structurally invisible to Enter, so a freshly
/// created Autopilot row (no id, no `pmloop-`, no `pmchat-`, no `driver.json`) fell into
/// the lease probe and — whenever it won the free per-session `driver.lock`, i.e. in the
/// window before pmd's first sweep grabs it — CREATE-and-chatted a second claude. That
/// live `pmchat-` then DEADLOCKED the daemon: `JobScheduler::drive`'s gate 1
/// (`human_present` -> `chat_lock::is_active`) returns `Monitoring` BEFORE gate 2's
/// `ensure_session`, so the `pmloop-` agent the human actually asked for was never
/// launched and the ledger id never pinned — for up to `chat_lock::CHAT_STALE_S` (12h).
///
/// Deciding on the tier FIRST removes the coin flip: the Autopilot branch never reaches
/// the lease probe, so `first_wake_action`'s `CreateAndChat` is unreachable from it.
pub(crate) fn first_enter_route(tier: Tier) -> FirstEnterRoute {
    match tier {
        Tier::Autopilot => FirstEnterRoute::EnsureDaemonThenArm,
        Tier::Standard => FirstEnterRoute::LeaseKeyed,
    }
}

/// What Enter does on a never-woken **Autopilot** row, once the daemon ensure has run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutopilotFirstEnter {
    /// A daemon is up ⇒ ARM the auto-attach (pmd's `ensure_session` mints+pins the id
    /// and launches the `pmloop-` agent; the idle drain then attaches it). Carries the
    /// status.
    Arm(String),
    /// No daemon and we could not start one ⇒ do NOT arm. An arm here would be a lie:
    /// nothing would ever pin an id, so the human would sit behind an encouraging status
    /// forever — the exact complaint this slice fixes. Carries an honest refusal that
    /// names `m` (switch to Standard) as the escape hatch.
    Refuse(String),
}

/// Decide the Autopilot never-woken Enter from the daemon ensure. PURE so both the
/// route and the human copy are unit-tested without spawning a process.
///
/// The ensure is a PRECONDITION of arming, not a race: this branch is only reached
/// after `first_enter_route` chose it, so there is no lease probe to disagree with.
///
/// Copy notes: `keybar_line` caps a transient status at a THIRD of the bar and
/// truncates the TAIL, so the important word is front-loaded and the `Refuse` reason is
/// last — the reason is the right thing to lose on a narrow pane, the `m` hint is not.
/// And the arm promises "you'll drop in", not "you WILL land in": `move_sel` disarms on
/// navigation, so a human who arrows away right after Enter has to press Enter again
/// (which then attaches via the `loop_alive` pre-empt). pmd is driving either way.
pub(crate) fn autopilot_first_enter(id: &str, ensure: &DaemonEnsure) -> AutopilotFirstEnter {
    if ensure.is_up() {
        AutopilotFirstEnter::Arm(format!(
            "{id}: autopilot is starting the agent — you'll drop in"
        ))
    } else {
        AutopilotFirstEnter::Refuse(format!(
            "{id}: autopilot needs pmd — press m for Standard to chat now ({ensure})"
        ))
    }
}

/// The armed auto-open transition (see [`App::pending_first_chat`]), decided from
/// the re-read ledger's `conversation_id` (via [`effective_id`]) and `run`. PURE so
/// the transition is unit-tested without disk/tty.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArmedOpen {
    /// Cleanly chattable (an id AND a parked `Monitoring`/`Idle` run) ⇒ open the
    /// resume chat on this id and disarm.
    Open(String),
    /// Parked `Blocked` ⇒ disarm and point the human at `a` (never auto-open a REPL
    /// on a parked-blocked session; a codex capture-failure lands here too).
    Blocked,
    /// A wake is `Running`, or no id has appeared yet ⇒ stay armed.
    Stay,
}

/// Decide the armed auto-open transition: `Blocked` routes to the answer prompt
/// regardless of id; otherwise a parked (`Monitoring`/`Idle`) run WITH an id opens
/// the chat, and anything else (a `Running` wake, or a parked run with no id yet)
/// stays armed.
pub(crate) fn armed_open_decision(effective_cid: Option<&str>, run: &job::JobRun) -> ArmedOpen {
    match run {
        job::JobRun::Blocked { .. } => ArmedOpen::Blocked,
        job::JobRun::Monitoring { .. } | job::JobRun::Idle => match effective_cid {
            Some(cid) => ArmedOpen::Open(cid.to_string()),
            None => ArmedOpen::Stay,
        },
        job::JobRun::Running { .. } => ArmedOpen::Stay,
    }
}

/// What an ARMED first-open drain actually DOES, once the ledger transition
/// ([`armed_open_decision`]), the tier and the `pmloop-` liveness are all known.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArmedRoute {
    /// Attach the daemon's live `pmloop-` agent session (BOTH tiers, whenever it is
    /// alive) and disarm.
    AttachLoop,
    /// Launch/resume the human's OWN `pmchat-` REPL on this conversation and disarm.
    /// **Standard only** — see [`armed_route`].
    Chat(String),
    /// Parked `Blocked` ⇒ disarm and point at `a`.
    Blocked,
    /// Keep waiting (still armed).
    Stay,
}

/// Whether an armed drain must PROBE tmux for a live `pmloop-` this tick.
///
/// Autopilot ALWAYS does, because there the live agent session outranks the ledger
/// transition entirely — same rule as `request_attach`'s `loop_alive` pre-empt, which
/// attaches an alive session regardless of `run`. Without this an armed Autopilot row
/// whose ledger read `Running` would sit `Stay` in front of a perfectly live agent and
/// then time out. It costs one `tmux has-session` per ~500ms tick, only while armed, and
/// the Autopilot arm is bounded.
///
/// Standard probes only on `Open`, exactly as it always has: `Blocked`/`Stay` cannot act
/// on the answer, and keeping the probe condition unchanged keeps Standard's drain
/// byte-identical.
pub(crate) fn armed_probes_loop(tier: Tier, decision: &ArmedOpen) -> bool {
    tier == Tier::Autopilot || matches!(decision, ArmedOpen::Open(_))
}

/// Route an armed drain: **attach-or-stay-armed on Autopilot, attach-or-chat on
/// Standard.**
///
/// The second half of this slice's invariant. Arming is how the Autopilot Enter avoids
/// creating a second writer — but the arm used to resolve through a `pending_chat`
/// FALLBACK, so the moment pmd pinned an id the drain would fresh-launch a `pmchat-` on
/// the very conversation pmd had just minted. That re-creates the exact state we are
/// eliminating (two claude on one id, and gate 1 deadlocking the daemon), just ~500ms
/// later. On Autopilot there is therefore no chat fallback at all: either the agent's own
/// session is live and we attach it, or we keep waiting for it.
///
/// Standard keeps the fallback verbatim: with pmd down, a Standard row that gained an id
/// has no `pmloop-` coming, and chatting it is exactly what the human asked for.
///
/// `loop_alive` means "a live `pmloop-` was OBSERVED" (see [`armed_probes_loop`] for when
/// the caller looks) and outranks the ledger transition — a live agent session is always
/// the thing to attach. `Blocked` outranks even that: never auto-open anything on a
/// parked-blocked session (a codex capture-failure `Capability` stop lands here too);
/// `a` answers it, and a deliberate Enter still attaches via `request_attach`.
pub(crate) fn armed_route(tier: Tier, decision: ArmedOpen, loop_alive: bool) -> ArmedRoute {
    match decision {
        ArmedOpen::Blocked => ArmedRoute::Blocked,
        _ if loop_alive => ArmedRoute::AttachLoop,
        // No live `pmloop-` yet. Autopilot WAITS for one (pmd is the writer);
        // Standard opens the chat (pmd-down fallback).
        ArmedOpen::Open(cid) => match tier {
            Tier::Autopilot => ArmedRoute::Stay,
            Tier::Standard => ArmedRoute::Chat(cid),
        },
        ArmedOpen::Stay => ArmedRoute::Stay,
    }
}

/// Status after returning from a unified project terminal.
pub(crate) fn chat_return_status(label: &str, still_live: bool) -> String {
    if still_live {
        format!("detached from {label} (still running)")
    } else {
        format!("{label}'s terminal ended")
    }
}

/// Label-free status suffix for a just-created terminal.
pub(crate) fn chat_park_note(still_live: bool) -> &'static str {
    if still_live {
        "running — Enter to drive"
    } else {
        "terminal ended"
    }
}

/// What a "still waiting" armed drain tick should do about the DAEMON (as opposed to
/// about the ledger, which is [`armed_route`]'s job).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmedWait {
    /// The daemon looks fine (or we cannot tell) — keep waiting.
    KeepWaiting,
    /// pmd has been observed DOWN long enough to be believed: give up and say so.
    DisarmDaemonDown,
}

/// PURE: may an armed auto-open give up because pmd is DOWN?
///
/// Two guards, both deliberate:
///
/// * `Unknown` NEVER disarms. A failed probe is not an outage, and inventing one would
///   be the same class of lie as the encouraging status this replaces.
/// * A SINGLE `Down` sample is not enough. `ensure_daemon` spawns pmd and returns
///   immediately, so there is a legitimate window — process spawn, dynamic linking,
///   `create_dir_all`, then `try_acquire` — in which no daemon holds the lock yet and
///   the honest answer is still "starting". Requiring
///   [`DAEMON_DOWN_DISARM_SAMPLES`] CONSECUTIVE fresh samples turns that into a real
///   time bound: samples are taken at most once per [`DAEMON_PROBE_TTL`], so this is
///   ≈9s of UNBROKEN observed-DOWN before anything is claimed — three orders of
///   magnitude over a normal boot, and still far tighter than claude's ~30s drain bound
///   (and the FIRST bound codex has ever had). Any `Up`/`Unknown` sample resets the
///   streak, so a daemon that comes up late is never punished for a slow start.
pub(crate) fn armed_wait_decision(live: DaemonLive, down_streak: u32) -> ArmedWait {
    match live {
        DaemonLive::Down if down_streak >= DAEMON_DOWN_DISARM_SAMPLES => {
            ArmedWait::DisarmDaemonDown
        }
        _ => ArmedWait::KeepWaiting,
    }
}
