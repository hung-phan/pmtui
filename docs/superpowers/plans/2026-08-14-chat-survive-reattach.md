# Design: SURVIVE-on-detach + re-attach for on-demand chat (agent-deck parity)

Status: FINAL / implementer-ready. Supersedes the draft of the same name. All
Critical/Important critic holes are folded in; dismissed items are listed in §8.

## 0. Goal & target UX

Make agent-manager's on-demand chat behave exactly like agent-deck's Enter-attach:

- **Ctrl+q** DETACHES from the interactive `claude`/`codex` REPL; the `pmchat-…`
  tmux session KEEPS RUNNING (detach ≠ kill).
- **Enter** again RE-ATTACHES to that same live session (no relaunch).
- The session ENDS only by (a) `/exit`·Ctrl+D inside the engine (its only pane dies →
  session dies), (b) crash, (c) an explicit pmtui kill (`d` or the new `K`), or
  (d) a bounded orphan-reap of a *detached-and-forgotten* session past the cap.
- Coexists with the ephemeral per-wake POLL that resumes the SAME `conversation_id`:
  **a live (attached or detached) chat REPL and a poll wake must NEVER both drive one
  conversation id.**

Today the interlock (`chat_lock`) keys the poll-defer on the **pmtui pid's `/proc`
liveness**. Under SURVIVE the pmtui process detaches while the tmux REPL lives on, so
pid-liveness is the wrong signal. The interlock re-keys on the **tmux chat session's
liveness**, observable by both pmd and pmtui via the deterministic name
`tmux::chat_session_name(id, root)` on the shared `-L <socket>` server.

### The single most important correctness rule

`chat_lock::is_active` is the poll-defer fork fence. It now shells out to tmux (a
genuinely fallible probe) and became a stateful self-healer (it may clear a marker and
reap an orphan). Every branch is chosen so the **fail-safe direction is DEFER**:

- Liveness **UNKNOWN** (probe `Err`) ⇒ DEFER (never resume on an unknown).
- Session **ALIVE** ⇒ DEFER (never resume a live REPL's id) — except a
  detached-and-forgotten reap, which still defers the tick it kills on.
- Session **CONFIRMED DEAD** ⇒ resume — unless a **fresh marker** says a launch is
  in flight (mark-before-launch window), in which case DEFER for a short grace.

The old predicate failed OPEN on a stale/absent marker (`_ => false`, resume). The new
one fails CLOSED on ambiguity (`_ => true`, defer) and only ever resumes on a *confirmed
dead* session or a *confirmed-dead-and-not-launching* one. That inversion is the whole
game.

---

## 1. New `Driver` capabilities (tmux.rs) — attachment + session age

The reap must (a) never kill a session a human is attached to, and (b) bound the poll's
deferral even when the on-disk marker is lost. Neither is derivable from
`is_alive`/`terminate`. Add two probes to the `Driver` trait (`src/tmux.rs:37-58`), both
with **fail-safe default bodies** so any future `Driver` impl compiles and defaults to
the never-reap direction:

```rust
use crate::clock::Epoch;   // add to tmux.rs imports

pub trait Driver: Send + Sync {
    // … existing spawn_step / is_alive / capture_tail / terminate …

    /// How many tmux clients are attached to `session` right now. Distinguishes an
    /// IN-USE REPL (a human is attached) from a DETACHED-and-forgotten one, so the
    /// orphan-reap never kills a session someone is typing in. Default `Ok(true)` =
    /// "assume attached" = never reap (fail-safe).
    fn has_clients(&self, _session: &str) -> Result<bool> { Ok(true) }

    /// The wall-clock epoch the tmux session was CREATED (`#{session_created}`), or
    /// `None` if unknown/dead. Anchors orphan-reaping on the session's OWN age,
    /// independent of the on-disk marker (so a lost marker cannot wedge the poll).
    /// Default `Ok(None)` = "age unknown" = never reap (fail-safe).
    fn session_created(&self, _session: &str) -> Result<Option<Epoch>> { Ok(None) }
}
```

### 1.1 `TmuxDriver` impls (after `terminate`, `src/tmux.rs:463`)

```rust
fn has_clients(&self, session: &str) -> Result<bool> {
    let out = self.base()
        .arg("list-clients").arg("-t").arg(exact(session))
        .output().context("run tmux list-clients")?;
    // Dead session / no server ⇒ non-zero ⇒ no clients. A live but detached session
    // ⇒ success with EMPTY stdout ⇒ no clients. A client attached ⇒ non-empty stdout.
    Ok(out.status.success() && !out.stdout.is_empty())
}

fn session_created(&self, session: &str) -> Result<Option<Epoch>> {
    let out = self.base()
        .arg("display-message").arg("-p").arg("-t").arg(exact(session))
        .arg("#{session_created}")
        .output().context("run tmux display-message")?;
    if !out.status.success() { return Ok(None); }
    Ok(String::from_utf8_lossy(&out.stdout).trim().parse::<i64>().ok())
}
```

`#{session_created}` is unix seconds; `Epoch` is `i64` seconds — no conversion.

### 1.2 `FakeDriver` support (`src/tmux.rs:472-562`)

Extend `Inner` and add setters so unit tests drive every branch hermetically:

```rust
struct Inner {
    alive: HashMap<String, bool>,
    commands: HashMap<String, Vec<String>>,
    tails: HashMap<String, String>,
    write_on_is_alive: Option<(PathBuf, i32)>,
    clients: HashMap<String, bool>,        // NEW
    created: HashMap<String, Epoch>,       // NEW
    fail_is_alive: std::collections::HashSet<String>, // NEW: arm an is_alive Err
}
// setters:
pub fn set_clients(&self, s: &str, attached: bool)  // inner.clients.insert
pub fn set_created(&self, s: &str, at: Epoch)        // inner.created.insert
pub fn fail_is_alive(&self, s: &str)                 // inner.fail_is_alive.insert
```

- `is_alive`: if `fail_is_alive` contains the session, `return Err(anyhow!("is_alive probe failed"))` (BEFORE the `write_on_is_alive` block); else unchanged.
- `has_clients`: `Ok(*inner.clients.get(s).unwrap_or(&false))`.
- `session_created`: `Ok(inner.created.get(s).copied())`.
- `terminate`: unchanged (sets `alive=false`); also drop the session's `clients`/`created` entries so a reaped session reads dead+ageless next tick.

Defaults (`clients=false`, `created=None`) match the fail-safe trait defaults, so the
existing defer tests that only `set_alive(true)` still hit the plain DEFER arm.

---

## 2. `chat_lock` re-keyed on tmux liveness (self-healing defer) — `src/chat_lock.rs`

This is the CHOSEN poll-defer mechanism (single mechanism, minimal plumbing: `driver`
is already a parameter at all three call sites; the session name is computable from
fields the scheduler already holds).

### 2.1 Constants (`:26-29`)

```rust
/// Orphan-reap cap: a DETACHED, UNATTACHED chat session older than this is reaped so
/// the poll can never defer forever. Anchored on the tmux session's own age
/// (`session_created`), NOT the marker, so a lost marker can't defeat the cap.
pub const CHAT_STALE_S: i64 = 12 * 3600;

/// Launch-grace: `mark` is written BEFORE `launch_interactive`, so there is a brief
/// window where the marker exists but the session is not yet alive. During it the poll
/// MUST defer (else it resumes the id the human is about to launch on = fork). Sized to
/// comfortably cover engine startup; only consulted while the session is NOT alive.
pub const CHAT_LAUNCH_GRACE_S: i64 = 30;
```

### 2.2 `ChatMarker` + `mark` (`:40-51`)

Extend the marker to be self-describing. Back-compatible (the struct is deliberately NOT
`deny_unknown_fields`): an old reader ignores the new fields, a new reader tolerates an
old marker (missing fields default).

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMarker {
    pub pid: u32,                    // diagnostics only now (no longer the liveness key)
    pub since: Epoch,                // launch epoch — anchors the LAUNCH-GRACE check only
    #[serde(default)] pub session: String,  // the pmchat-… tmux session (self-describing)
    #[serde(default)] pub socket: String,   // the -L socket it launched on — DIAGNOSTIC ONLY
}

pub fn mark(paths: &ProjectPaths, pid: u32, session: &str, socket: &str, now: Epoch)
    -> anyhow::Result<()>
{
    std::fs::create_dir_all(paths.daemon_dir())?;
    state::write_json_atomic(&paths.chat_lock(),
        &ChatMarker { pid, since: now, session: session.into(), socket: socket.into() })
}
```

`clear` (`:55-57`) unchanged. `socket` is recorded for log/diagnostics of a
socket-mismatch anomaly; it is **not** a fork guard (the fork guard for a socket
mismatch is the shared `"pmd"` default — see §7). `since` is used ONLY by the
launch-grace check; reaping no longer keys on it.

### 2.3 `is_active` (`:64-72`, full rewrite; `pid_alive` at `:76-78` DELETED)

Add `use crate::tmux::Driver;`.

```rust
/// Must the poll DEFER because a chat REPL owns this conversation right now?
///
/// SURVIVE model: the truth is the tmux chat session's liveness, NOT a pmtui pid — a
/// detached REPL keeps the session alive with NO pmtui process attached. This predicate
/// is self-healing and its ONLY resume paths are on a CONFIRMED-DEAD session:
///
///   is_alive == Err(_)   liveness UNKNOWN (spawn failure: ENOMEM/EAGAIN/RLIMIT/…).
///                         FAIL SAFE: return true (DEFER). NEVER clear the marker.
///   is_alive == Ok(false) session confirmed DEAD.
///                         - fresh marker (now-since < LAUNCH_GRACE_S) => a launch is in
///                           flight (mark-before-launch): DEFER (true); do NOT clear.
///                         - otherwise => the REPL ended (/exit·Ctrl+D·crash·kill) or the
///                           launch was abandoned: clear the marker, resume (false).
///   is_alive == Ok(true)  session ALIVE.
///                         - detached (has_clients==false) AND older than CHAT_STALE_S
///                           (by session_created) => ORPHAN. terminate() it, but DEFER
///                           THIS tick (true). The NEXT sweep sees Ok(false) and resumes
///                           cleanly — so there is NEVER a same-tick kill-then-resume
///                           concurrent-drive window, and is_active NEVER returns false
///                           while a session is alive (preserves §5.3's fork invariant).
///                         - otherwise (fresh / attached / age-unknown) => DEFER (true).
///
/// NOT pure: it may clear a marker or reap an orphan. Idempotent and fork-safe. Lives
/// here (not the callers) because all three call sites already hold `driver`.
/// Marker-INDEPENDENT reaping (age from tmux) means a lost/corrupt marker can never
/// wedge the poll: a live orphan is still reaped at the cap.
pub fn is_active(driver: &dyn Driver, session: &str, paths: &ProjectPaths, now: Epoch) -> bool {
    let marker = state::read_json_opt::<ChatMarker>(&paths.chat_lock()).ok().flatten();
    match driver.is_alive(session) {
        Err(_) => true,                                   // UNKNOWN => defer, keep marker
        Ok(false) => match &marker {
            Some(m) if now.saturating_sub(m.since) < CHAT_LAUNCH_GRACE_S => true, // launching
            _ => { clear(paths); false }                  // dead & not launching => resume
        },
        Ok(true) => {
            // Assume attached on any probe error (never reap on unknown).
            let attached = driver.has_clients(session).unwrap_or(true);
            let aged = driver.session_created(session).ok().flatten()
                .map(|c| now.saturating_sub(c) >= CHAT_STALE_S)
                .unwrap_or(false);                        // age unknown => not-aged => defer
            if aged && !attached {
                let _ = driver.terminate(session);        // reap the orphan …
                true                                      // … but DEFER this tick (resume next)
            } else {
                true                                      // fresh / attached => defer
            }
        }
    }
}
```

### 2.4 Module doc rewrite (`:16-19`)

Replace the "pure predicate — no side effects" paragraph with: *"`is_active` keys on the
tmux chat session's liveness (a detached REPL survives with no pmtui process). It is
self-healing, not pure: it clears the marker of a confirmed-dead session and reaps a
detached-and-forgotten one past `CHAT_STALE_S`. Every branch fails safe toward DEFER;
it resumes only on a confirmed-dead, not-launching session. The on-disk marker is a
launch-window bridge + diagnostics, NOT the liveness key."*

### 2.5 Unit tests (`:80-141`, rewrite over `FakeDriver`; drop every `#[cfg(target_os="linux")]`)

`use crate::tmux::fake::FakeDriver;`. `S = "pmchat-x"`, `NOW = 1_000_000`.

- `alive_session_defers`: `set_alive(S,true)` ⇒ `is_active(&d,S,&paths,NOW)` true.
- `unknown_liveness_defers_and_keeps_marker`: `mark(...)`, `fail_is_alive(S)` ⇒ true AND `chat_lock().exists()`.
- `dead_session_resumes_and_clears_marker`: `mark(...)`, S not alive, marker older than grace (`since = NOW - 60`) ⇒ false AND `!chat_lock().exists()`.
- `dead_but_fresh_marker_defers_launch_window`: `mark(...)` with `since = NOW`, S not alive ⇒ `is_active(NOW)` true (launch in flight); marker NOT cleared.
- `detached_orphan_past_cap_is_reaped_then_resumes`: `set_alive(S,true)`, `set_created(S, NOW-CHAT_STALE_S-1)`, `set_clients(S,false)` ⇒ `is_active(NOW)` true AND `d.is_alive(S)==Ok(false)` (terminated). Second call ⇒ false (resumes; marker cleared).
- `attached_session_never_reaped_past_cap`: as above but `set_clients(S,true)` ⇒ `is_active(NOW)` true AND `d.is_alive(S)==Ok(true)` (NOT terminated). (Closes "kills a human's live session".)
- `age_unknown_alive_session_defers`: `set_alive(S,true)`, no `created` ⇒ true, not terminated.
- `absent_marker_alive_session_defers_but_reaps_at_cap`: `set_alive(S,true)`, no marker, `set_created(S,NOW-CHAT_STALE_S-1)`, `set_clients(S,false)` ⇒ true AND terminated (marker-independent backstop; closes "markerless-alive wedges forever").

---

## 3. Wire the new defer into `JobScheduler` — `src/job_engine.rs`

The scheduler owns `project_id: String` (`:77`) and `work_dir: PathBuf` (`:79`); the chat
name is `tmux::chat_session_name(&self.project_id, &self.work_dir)` — byte-identical to
pmtui's `chat_session_name(&id, &e.root)` because `Runner::build` calls
`JobScheduler::new(p.id, p.root, &p.id, …)` (`daemon.rs:86-91`). `driver` is already in
scope at all three sites.

- `:234` (Idle arm) and `:246` (due-`Monitoring` arm) of `tick`:
  `crate::chat_lock::is_active(&self.paths, now)`
  → `crate::chat_lock::is_active(driver, &tmux::chat_session_name(&self.project_id, &self.work_dir), &self.paths, now)`
- `:792` (`on_blocked` answer-resume defer): same substitution (`driver` is `on_blocked`'s param).

Everything else at those arms is unchanged: a `true` result still returns a benign
`Monitoring`/`Escalated` WITHOUT spawning, mutating `self.run`, consuming budget, or
adopting; `:255-257` still pauses the wall-clock window while a chat is active.

### 3.1 Convert the 4 defer tests (`:1372-1564`)

They armed via `chat_lock::mark(pid)` + `/proc` (Linux-gated). Under SURVIVE they arm via
the fake driver's tmux liveness (what production keys on), so the cfg gate is dropped.
Compute the name once (the `tests` module is a child of `job_engine`, so it reads the
private fields): `let chat = tmux::chat_session_name(&fx.sched.project_id, &fx.sched.work_dir);`

- Arm defer: `fx.driver.set_alive(&chat, true);` (replaces `mark`).
- Release: `fx.driver.set_alive(&chat, false);` (replaces `clear`) — with no marker and
  a dead session, `is_active` hits `Ok(false) => _ => clear; false`, exactly the resume.
- Convert `chat_active_defers_the_idle_wake` (`:1374`), `chat_active_defers_a_due_monitoring_wake` (`:1411`), `chat_active_defers_the_blocked_answer_resume` (`:1458`), `chat_does_not_age_the_wall_clock_budget` (`:1526`).
- `dead_pid_marker_does_not_block_spawn` (`:1445`) → rename `dead_session_does_not_block_spawn`: don't set the chat alive (fake defaults false) ⇒ `Spawned{seq:0}`.
- Add `stale_orphan_reaps_and_resumes`: `set_alive(&chat,true)`, `set_created(&chat, START-CHAT_STALE_S-1)`, `set_clients(&chat,false)`; `tick` at `START` ⇒ deferred (Monitoring, no spawn) AND `!fx.driver.is_alive(&chat)`; a second `tick` ⇒ `Spawned` (orphan handed back on the next sweep).

### 3.2 daemon.rs adopt tests (`:1197-1233`)

- `agent_loop_adopts_registry_seed_on_first_wake`: keep the chat session NOT alive
  (FakeDriver default) so adopt still fires; assert `driver.is_alive(&chat).unwrap()==false`
  explicitly for intent.
- Add `agent_loop_defers_adopt_while_chat_session_alive`: `driver.set_alive(&chat_session_name("bot", dir.path()), true)` before the sweep ⇒ assert `driver.spawn_count()==0` and the ledger `conversation_id` stays `None` (no adopt while a live chat owns the seed).

---

## 4. pmtui: never kill on detach + re-attach routing — `src/bin/pmtui.rs`

### 4.1 `chat()` (`:2246-2301`)

Four coordinated edits. `mark` STAYS before launch (load-bearing — see §5.2).

**(a) `mark` call site (`:2248`)** — pass the session + socket:
```rust
chat_lock::mark(&req.session_paths, std::process::id(), &req.session, &req.socket,
                SystemClock.now())?;
```

**(b) `ChatGuard::drop` (`:2259-2263`)** — NEVER terminate; clear ONLY on confirmed-dead:
```rust
impl Drop for ChatGuard<'_> {
    fn drop(&mut self) {
        // SURVIVE: detach must leave the REPL running. Distinguish detach from a real
        // end by re-probing tmux. Clear the marker ONLY on a CONFIRMED-DEAD session
        // (Ok(false)); on ALIVE (detach) or UNKNOWN (Err) leave it, so the poll keeps
        // deferring on the live session. We NEVER terminate here — ending is user-driven.
        if matches!(self.driver.is_alive(self.session), Ok(false)) {
            chat_lock::clear(self.paths);
        }
    }
}
```

**(c) Remove the pre-launch terminate (`:2283`).** Deleting `let _ = driver.terminate(&req.session);`
makes `launch_interactive` idempotent-by-name: if the `pmchat-…` session is alive (a
prior detach) it re-binds Ctrl+q and returns `Ok` without a new REPL, so the following
`attach_command().status()` (`:2296`) RE-ATTACHES the identical live session.

**(d) Re-attach vs fresh-launch guard (replaces the TOCTOU block `:2271-2278`).** Add a
`reattach: bool` field to `ChatReq` (§4.3). Gate as follows:
```rust
let driver = TmuxDriver::with_socket(&req.socket);
// If we intended to RE-ATTACH a live session (probed alive at Enter) but it is now
// CONFIRMED DEAD (e.g. reaped, or /exit'd from another attach), do NOT fresh-relaunch —
// a fresh `--resume <cid>` here would fork against the poll that is about to resume it.
// Hand back cleanly; the poll takes over on its next sweep.
if req.reattach && matches!(driver.is_alive(&req.session), Ok(false)) {
    anyhow::bail!("the chat ended — the poll will resume it");
}
// Only a FRESH launch can race a just-started wake (a re-attach can't: pmd defers on the
// live session). Gate the launch-time fork guard behind "not yet alive" so re-attach
// never trips it.
if !driver.is_alive(&req.session).unwrap_or(false)
    && let Ok(Some(ledger)) = job::load(&req.session_paths)
    && matches!(ledger.run, job::JobRun::Running { .. })
{
    anyhow::bail!("a wake just started — try again in a moment");
}
driver.launch_interactive(&req.session, &req.root, &req.argv)?;  // idempotent: attach if alive
```

### 4.2 `create_chat()` (`:2333-2376`)

Same shape; the lease-drop ordering is preserved (guard body runs, then `_lease` drops
LAST — see §5.1). No `reattach` field: create is always the FIRST create (a re-Enter of a
survived create-chat routes through `chat()` because `effective_id` now returns the
registry seed — §4.4).

**(a) `mark` (`:2335`)**: `chat_lock::mark(&req.session_paths, std::process::id(), &req.session, &req.socket, SystemClock.now())?;`

**(b) `CreateGuard::drop` (`:2346-2351`)** — never terminate; clear only on confirmed-dead:
```rust
impl Drop for CreateGuard {
    fn drop(&mut self) {
        if matches!(self.driver.is_alive(&self.session), Ok(false)) {
            chat_lock::clear(&self.paths);
        }
        // `_lease` drops here, AFTER this body — releasing the flock last (fork fence).
    }
}
```

**(c) Remove the pre-launch terminate (`:2364`).** Same reasoning as §4.1c.

### 4.3 `ChatReq` gains `reattach` (`:208-224`); set at both build sites

Add `reattach: bool` to `ChatReq` with a doc: *"true when the chat session was probed
ALIVE at Enter (a re-attach). `chat()` refuses to fresh-relaunch a re-attach whose session
died between probe and launch, so it never forks the id the poll is about to resume."*

- `request_attach` Chat arm (`:744-751`): `reattach: chat_alive` (the §4.4 probe).
- `drain_armed_first_chat` (`:389-398`): `reattach: false` (an armed first-chat targets a
  never-launched session; if a live one existed the Enter would have routed here as a
  re-attach instead).

### 4.4 Live-chat probe + router extension (`request_attach` `:715-736`, `agent_loop_enter` `:1000-1013`)

A live (detached) chat must win the Enter dispatch REGARDLESS of `run_is_running`
ordering, so a transient ledger `Running` can't strand the human's own session. After the
owned inputs are extracted and BEFORE the `agent_loop_enter` dispatch (`:736`), probe:
```rust
let chat_session = chat_session_name(&id, &root);
let chat_alive = TmuxDriver::with_socket(&self.socket)
    .is_alive(&chat_session).unwrap_or(false);
```
Extend the pure router with a leading `chat_session_live: bool` and a leading branch:
```rust
fn agent_loop_enter(
    chat_session_live: bool,
    live_pane: Option<&str>,
    conversation_id: Option<&str>,
    run_is_running: bool,
) -> EnterAction {
    // A live (detached) chat REPL always re-attaches — the poll is deferring on it, so a
    // transient ledger `Running` must not strand the human's own session. Mutually
    // exclusive with a live watch pane in practice, but ordered first defensively.
    if chat_session_live && let Some(id) = conversation_id {
        return EnterAction::Chat(id.to_string());
    }
    if let Some(pane) = live_pane {
        return EnterAction::Watch(pane.to_string());
    }
    match conversation_id {
        Some(id) if !run_is_running => EnterAction::Chat(id.to_string()),
        None => EnterAction::WaitingFirstWake,
        _ => EnterAction::NoWake,
    }
}
```
Update the call at `:736` to pass `chat_alive` first. `cid` for the `Chat` arm is
`effective_id(ledger_cid, registry_cid)` — `Some` for every live `pmchat` (a resume-chat
has a ledger cid; a create-chat has a registry seed), so the router only returns `Chat`
when the id is present. Because `effective_id` returns the registry seed once a
create-chat has been seeded, a re-Enter of a **survived create-chat** routes to `Chat`
(→ `chat()`, `reattach=true`) and re-attaches by name — never re-entering `create_chat`.

### 4.5 Doc-comment invariant inversions

Rewrite these now-wrong comments to the SURVIVE invariant *"On detach, leave the session
and marker alive; the poll defers on tmux liveness. The marker is cleared only when the
REPL is confirmed dead (is_alive == Ok(false)). Ending is user-driven (`/exit`·Ctrl+D),
an explicit pmtui kill (`d`/`K`), or a bounded orphan-reap — never a side effect of
detach."*:
- `chat()` header (`:2222-2229`) and `create_chat()` header (`:2320-2324`).
- `ChatReq.session` (`:221-223`) and `CreateChatReq.session` (`:250-252`) field docs
  (they still say "terminates it on return").
- `run()` drain comment for `pending_chat` (`:2088-2092`) ("TERMINATES the chat tmux session").
- **`tmux::chat_session_name` doc (`src/tmux.rs:197-198`)**: the parenthetical "(the
  launcher terminates it on return)" is the exact invariant SURVIVE inverts — rewrite to
  "(the session survives detach; it is ended by `/exit`·Ctrl+D·crash, an explicit pmtui
  kill, or a stale-orphan reap)".

### 4.6 Tests

- Update the 4 `agent_loop_enter` unit tests (`:3288-3324`) to pass a leading `false`.
- Add `agent_loop_enter_reattaches_live_chat_over_transient_running`:
  `agent_loop_enter(true, None, Some("conv-1"), true) == Chat("conv-1")`.
- Add `agent_loop_enter_live_chat_ignored_when_no_id`:
  `agent_loop_enter(true, None, None, false) == WaitingFirstWake` (falls through).
- `chat`/`create_chat` themselves stay untested (they own the tty/tmux, per the existing
  convention documented in their headers). The `reattach` bail is exercised via the pure
  router + the `tmux_available()`-gated integration test pattern already in the repo
  (`begin_delete_confirms_a_live_session_despite_a_stale_idle_flag` `:3506`) if desired.

---

## 5. Create-path & fork-fence proof (Q3/Q4)

### 5.1 The fence legitimately moves

**Old:** the `ProjectLease` is moved into `CreateGuard._lease` (`:2357`) and held for the
whole REPL, so pmd cannot `reconcile_one`→tick→adopt until the human's conversation is
persisted.

**New:** the lease is held only for the `create_chat` call; it releases on **detach**
(guard body runs, then `_lease` drops last), while the `pmchat-…` session lives on. The
fence that prevents a concurrent resume shifts to the `is_active`-defer keyed on the live
session. Proof adopt/tick-defer stay correct:

1. **During attach:** pmtui holds the exclusive `driver.lock`; pmd's `reconcile_one`
   (`daemon.rs:265-287`) cannot acquire the lease ⇒ skips the tick ⇒ no spawn/adopt.
   (Double fence: lease + would-be `is_active`.)
2. **On detach:** guard body runs (no kill; marker kept because `is_alive==Ok(true)`),
   then `_lease` drops ⇒ flock free. The `pmchat-…` session is alive and the marker
   (written at `:2335`) is intact.
3. **Next pmd sweep:** `reconcile_one` re-acquires the lease, re-sets the registry seed
   (`daemon.rs:95`), then `tick` hits `JobRun::Idle` (`:233`). `is_active(driver,
   chat_session,…)` sees the session ALIVE ⇒ `true` ⇒ `tick` returns `Monitoring{until:now}`
   WITHOUT spawning or adopting (`:234-241`). **No fork.**
4. **Adopt only after the session ends:** when the human `/exit`s (or the orphan is
   reaped at the cap, §2.3), the NEXT sweep's `is_active` ⇒ `false` ⇒ `tick`→`spawn`→adopt
   arm (`:348-358`).
   - Chatted: `claude` persisted the `--session-id <seed>` conversation before the pane
     died, so `claude --resume <seed>` succeeds → clean wake → `adopted_unverified`
     cleared (`:366`). Correct hand-off.
   - Never persisted (created, never messaged, Ctrl+D/crash): resume fails "No
     conversation found with session ID: <seed>" and the ANCHORED de-poison
     (`after_failure`, `:846-860`) discards the seed and eager-respawns a fresh minted id.
     Correct — the de-poison net still covers this.

### 5.2 The launch-window fork is closed by mark-before-launch + grace (CRITICAL fix)

The resume-`chat()` path holds **no lease** (pmd holds the per-runner lease
*continuously* once acquired — `daemon.rs:61-63` — so pmtui can never take it while pmd
is up). Its ONLY fence is the `is_active`-defer. The draft regressed this: with the naive
"`!is_alive` ⇒ clear + false" predicate, a pmd tick landing in the window between pmtui's
`mark` and the session coming alive would see a dead session, clear the human's marker,
and spawn a resume of the same id = fork.

Closed by two things working together:
- `mark` STAYS before `launch_interactive` (so the marker is present the instant the
  window opens), and
- `is_active`'s `Ok(false)` arm DEFERS while the marker is fresh (`now-since <
  CHAT_LAUNCH_GRACE_S`) — treating a fresh marker on a not-yet-alive session as
  "launch in flight". Once the session is alive, the `Ok(true)` arm defers normally.

So from the moment pmtui marks, every pmd tick defers — through the launch and the whole
attach. A failed launch (missing engine → `?`) runs the guard, which clears the marker on
`Ok(false)`, so no stuck defer.

### 5.3 The reap preserves the "never Running under a live chat" invariant

`is_active` returns `false` ONLY on a **confirmed-dead** session (the `Ok(false)` arm). The
reap arm terminates then returns `true` (defer this tick); the poll resumes only on the
NEXT sweep, once `is_alive==Ok(false)`. Therefore `is_active` NEVER returns false while a
session is alive, so `tick` never reaches `spawn`/writes `Running` under a live chat — the
draft's §3.5 invariant holds even through a reap, and there is no same-tick
kill-then-`--resume` concurrent-drive window.

### 5.4 Residual (documented, accepted — pre-existing)

A FRESH resume launch has a sub-ms cross-process TOCTOU: pmd's `is_active` check (marker
still absent) → pmd writes `Running`, racing pmtui's `mark` → ledger re-read → launch. The
§4.1d ledger-`Running` gate catches the common case (pmd's write landed before pmtui's
read); the true sub-ms residual is the same one the current code accepts (`chat()` header
`:2234-2235`). It CANNOT be lease-fenced (pmd holds the lease continuously); mark-before-
launch + grace shrinks the exposure to that sub-ms window and never widens it. Accepted.

---

## 6. Ending the chat & explicit kills (Q5)

### 6.1 Primary end: `/exit`·Ctrl+D (or crash)

The engine tears down its only pane → the `pmchat-…` session dies →
`attach_command().status()` returns → the drop-guard probes `is_alive==Ok(false)` →
clears the marker. pmd's next sweep: `is_active==false` → the poll resumes the saved id.
No "detect exit vs detach" logic beyond the `is_alive` re-probe.

### 6.2 Explicit delete (`d`) — kill the surviving session — `begin_delete` `:869-874`

Today the AgentLoop branch sets `Confirming { id, session: String::new() }`, so
`remove_project`'s `terminate("")` is a no-op — fine under the OLD model (chat was killed
on return). Under SURVIVE the `pmchat-…` session survives, so delete must name+kill it:
```rust
if mode == Mode::AgentLoop {
    // SURVIVE: the chat REPL now outlives a detach, so an explicit close must END it.
    // (The daemon prune-abort still kills the in-flight pmj- worker on the next sweep;
    // that path is unchanged.)
    self.mode = UiMode::Confirming { id, session: chat_session_name(&e.id, &e.root) };
    return;
}
```
On confirm (`:1978-1981`), `remove_project(id, session)` runs
`TmuxDriver::with_socket(&self.socket).terminate(session)` (`:896`) → kills the surviving
`pmchat-…`, then drops the registry row. Also explicitly clear the chat marker in
`remove_project` for a removed AgentLoop id (so a reused id at the same root never inherits
a stale marker even if the kill lags): after the row is retained-out, if the removed
entry's `mode` was AgentLoop, `chat_lock::clear(&ProjectPaths::for_session(&root, id))`.

### 6.3 New key `K` — end the chat REPL but KEEP the session — `end_chat`

`K` (capital; free — Normal mode uses `q j k n p t d a Enter` only) force-ends a detached
chat and hands the id straight back to the poll, without deleting the row. Add to the
Normal handler (`:1990-2001`): `KeyCode::Char('K') => app.end_chat(),` and:
```rust
/// `K`: end the live chat REPL for the selected agent-loop session (kill the pmchat-
/// pane + clear the marker), leaving the registry row and ledger intact so the poll
/// resumes the same conversation on its next sweep. No-op on non-AgentLoop rows or when
/// no chat is live.
fn end_chat(&mut self) {
    let Some(v) = self.selected_view() else { return; };
    let (mode, id) = (v.mode, v.id.clone());
    let reg = Registry::load(&self.registry_path).unwrap_or_default();
    let Some(e) = reg.projects.iter().find(|p| p.id == id) else { return; };
    let Some(session) = end_chat_target(mode, &id, &e.root) else {
        self.status = "K ends a live chat (this row has none)".into();
        return;
    };
    let driver = TmuxDriver::with_socket(&self.socket);
    if matches!(driver.is_alive(&session), Ok(true)) {
        let _ = driver.terminate(&session);
        chat_lock::clear(&entry_state_paths(e));      // hand back to the poll immediately
        self.status = format!("ended chat {id} — the poll resumes it");
    } else {
        self.status = format!("{id}: no live chat to end");
    }
    self.refresh();
}
```
Factor the decision into a PURE, unit-testable helper (this is how Task-4 side effects
are tested without adding a `Driver` to `App` — see §6.4):
```rust
/// The pmchat- session name to end for an agent-loop row, or None if the row can't have
/// a live chat (non-AgentLoop). Pure: no tmux, no disk.
fn end_chat_target(mode: Mode, id: &str, root: &Path) -> Option<String> {
    (mode == Mode::AgentLoop).then(|| chat_session_name(id, root))
}
```
Add `K End chat` to the Normal-mode footer legend (`render_keybar` chips, `:1917-1926`).

### 6.4 Orphan-proof external removal — daemon prune-abort — `daemon.rs:169-177`

The prune loop aborts the `pmj-` worker but has no knowledge of the surviving `pmchat-`.
Under SURVIVE a row removed OUT-OF-BAND (external registry edit/tooling — not pmtui's `d`)
would leave the `pmchat-` REPL running forever with no owner (its runner is pruned, so
`is_active` is never consulted again). Fix: for a pruned AgentLoop runner, also terminate
its deterministic chat session (id from the map key, root from `Runner.root`):
```rust
for id in removed {
    if let Some(mut r) = self.runners.remove(&id) {
        match &mut r.driven {
            Driven::Reference(s) => s.abort(driver, clock),
            Driven::Native(s)    => s.abort(driver, clock),
            Driven::Job(s) => {
                s.abort(driver, clock);
                // SURVIVE: a Job runner's chat REPL outlives detach; an externally
                // removed row would orphan it. Kill it by deterministic name.
                let _ = driver.terminate(&crate::tmux::chat_session_name(&id, &r.root));
            }
        }
    }
}
```
(`Driven::Job` ⇒ AgentLoop by construction, `daemon.rs:85-96`.)

---

## 7. Invariants preserved (Q6)

- **Fork fence (layered):** (i) create-path holds the lease during attach; (ii) after
  detach, `is_active`-defer-on-live-session prevents any resume; (iii) mark-before-launch
  + `CHAT_LAUNCH_GRACE_S` covers the launch window; (iv) the §4.1d ledger-`Running` gate
  covers the fresh-launch race; (v) the `reattach` bail stops a re-attach from
  fresh-relaunching a just-reaped session; (vi) `is_active` fails SAFE (DEFER) on an
  UNKNOWN probe and only resumes on a CONFIRMED-DEAD session.
- **Bounded deferral / self-heal:** a confirmed-dead session's marker is cleared by both
  `is_active` (pmd) and the drop-guards (pmtui). A detached-and-forgotten live session is
  reaped at `CHAT_STALE_S` anchored on the tmux session's OWN age (marker-independent), so
  a lost/corrupt marker can never wedge the poll. An attached (in-use) session is never
  reaped.
- **Single-writer ledger preserved.** pmd remains the SOLE writer of the per-session
  ledger and `state.json`; pmtui writes only the registry seed, the chat marker, and
  answers; the REPL writes only the engine-owned conversation JSONL. `is_active` writes
  only the marker (clear) and the tmux session (reap) — never the ledger.
- **Deterministic-name + shared-socket coupling** (`daemon.rs:86-91` builds
  `JobScheduler::new(p.id, p.root, …)`) so `chat_session_name(&self.project_id,
  &self.work_dir)` == pmtui's `chat_session_name(&id, &e.root)`; any future `session_id !=
  id` divergence would desync the interlock — called out in the `is_active` doc.
- **Socket precondition (hard):** pmd and pmtui MUST share `-L <socket>` (both default
  `"pmd"`: `pmd.rs:38`, `parse_socket_arg:65`). A mismatch re-opens the fork hole
  (pmd's `is_alive` queries a different server, never sees the REPL). `ChatMarker.socket`
  records the launch socket for DIAGNOSTICS ONLY (surfaced in logs on a reap/anomaly); it
  is NOT a fork guard — the guard is the shared default. Document in the `chat_lock`
  module doc and near `parse_socket_arg`.
- **codex scope (validation gate, not code):** create-chat is claude-only
  (`build_chat_create` `unreachable!`s for codex, `:1220`). A surviving codex chat is
  `codex resume <cid>`. BEFORE enabling codex resume-chat under SURVIVE, live-validate:
  (1) `codex resume` is a persistent interactive TUI that survives tmux detach/reattach;
  (2) `codex resume <cid>` continues `cid` in place rather than forking a new rollout id.
  If it forks, capture the new id back into the ledger on chat end or block codex
  resume-chat under SURVIVE. Track as a pre-ship checklist item.

---

## 8. Dismissed / downgraded critic findings (with reasons)

- **"Lease-fence the resume-`chat()` launch" (critic 1, Minor).** Dismissed as
  infeasible: pmd holds the per-runner lease CONTINUOUSLY once acquired
  (`daemon.rs:61-63`), so pmtui can never acquire it while pmd is up. The launch window is
  instead closed by mark-before-launch + `CHAT_LAUNCH_GRACE_S` (§5.2); the sub-ms residual
  is the pre-existing, accepted TOCTOU (§5.4).
- **"`ChatMarker.socket` provides zero socket-mismatch detection" (critic 1, Minor).**
  Accepted the fact, dismissed the implied requirement to make it a guard: the field is
  explicitly DIAGNOSTIC-ONLY; the socket-mismatch fork guard is the shared `"pmd"` default
  (§7). The doc no longer claims it makes the mismatch "detectable" as a fence.
- **"Verify `terminate` succeeded before dropping the row" (critic 2, Minor).** Partially
  adopted: `K` and `d` explicitly `clear` the marker (not relying on dead-session
  self-heal), but the `terminate` call itself stays best-effort (its result is `Result<()>`
  that all repo call sites ignore — a re-probe/retry loop in the UI thread is out of scope
  and would block the TUI). A lingering session after a slow kill is bounded by the
  orphan-reap; a reused id self-heals via the marker-independent age reap (§2.3).

---

## 9. Task breakdown (SDD, 4 tasks)

### Task 1 — Driver attachment/age probes + `chat_lock` re-key
**Files:** `src/tmux.rs`, `src/chat_lock.rs`.
**Changes:** add `Driver::has_clients` + `Driver::session_created` with fail-safe default
bodies (§1); implement on `TmuxDriver` (`list-clients` / `display-message
#{session_created}`); extend `FakeDriver` with `clients`/`created`/`fail_is_alive` +
setters (§1.2). In `chat_lock`: add `CHAT_LAUNCH_GRACE_S`; extend `ChatMarker`
(+`session`,+`socket`, `#[serde(default)]`); change `mark` to `(paths, pid, session,
socket, now)`; rewrite `is_active` to `(driver, session, paths, now)` with the three-way
`is_alive` match + launch-grace + marker-independent client-gated defer-one-tick reap
(§2.3); delete `pid_alive`; rewrite the module doc (§2.4).
**Tests:** the 8 `chat_lock` cases in §2.5 (all over `FakeDriver`, no cfg gates), covering
UNKNOWN-defers, launch-grace-defers, confirmed-dead-resumes, orphan-reap-then-resume,
attached-never-reaped, and markerless-alive-still-reaps.

### Task 2 — Wire the defer into `JobScheduler` + orphan-proof prune
**Files:** `src/job_engine.rs`, `src/daemon.rs`.
**Changes:** update the 3 `is_active` sites (`:234`,`:246`,`:792`) to pass `driver` +
`tmux::chat_session_name(&self.project_id, &self.work_dir)` (§3); add the AgentLoop
`terminate(chat_session_name)` to the prune-abort loop (§6.4).
**Tests:** convert the 4 defer tests (`:1372-1564`) to `set_alive`/`set_alive(false)`,
drop cfg gates, rename `dead_pid_marker_…`→`dead_session_does_not_block_spawn`, add
`stale_orphan_reaps_and_resumes` (§3.1); add `daemon.rs`
`agent_loop_defers_adopt_while_chat_session_alive` + the explicit not-alive assert on the
existing adopt test (§3.2).

### Task 3 — `chat()`/`create_chat()` never kill on detach + re-attach routing
**Files:** `src/bin/pmtui.rs`, `src/tmux.rs` (doc only).
**Changes:** `ChatGuard`/`CreateGuard::drop` clear-only-on-`Ok(false)`, no `terminate`
(§4.1b/§4.2b); remove pre-launch `terminate` at `:2283`/`:2364` (§4.1c/§4.2c); add
`ChatReq.reattach` + the re-attach/fresh-launch gate replacing the TOCTOU block
(§4.1d/§4.3); update both `mark` call sites to pass `&req.session, &req.socket`
(§4.1a/§4.2a); add the `chat_alive` probe in `request_attach` + extend `agent_loop_enter`
with `chat_session_live` and update the `:736` call (§4.4); rewrite the inverted doc
comments incl. `tmux.rs:197-198` (§4.5).
**Tests:** update the 4 `agent_loop_enter` tests for the leading arg; add
`agent_loop_enter_reattaches_live_chat_over_transient_running` and
`agent_loop_enter_live_chat_ignored_when_no_id` (§4.6). (`chat`/`create_chat` stay
untested per convention.)

### Task 4 — Explicit kill actions (`d` kills the surviving session; new `K` ends chat)
**Files:** `src/bin/pmtui.rs`.
**Changes:** AgentLoop `begin_delete` branch names the chat session (§6.2) + `remove_project`
clears the marker for a removed AgentLoop id; add `KeyCode::Char('K') => app.end_chat()`,
the `end_chat` method + the pure `end_chat_target` helper (§6.3); add `K End chat` to the
footer legend.
**Tests (state + pure helper, no Driver injection):** `end_chat_target(AgentLoop, id,
root) == Some(chat_session_name(id,root))` and `== None` for Interactive/Auto;
`begin_delete_on_agent_loop_names_chat_session` — assert `app.mode == Confirming { session
}` where `session == chat_session_name(id, root)` (the existing
`confirming_close_of_agent_loop_removes_row_but_keeps_session_state` at `:4230` still
passes because `terminate` on the `"pm-test"` socket is a harmless no-op); `end_chat` on a
non-AgentLoop row sets the "this row has none" status and is a no-op; the tmux
terminate+clear itself is covered (optionally) by a `tmux_available()`-gated integration
test mirroring `begin_delete_confirms_a_live_session_despite_a_stale_idle_flag` (`:3506`).
