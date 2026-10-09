# Using pmtui day to day

How the supervisor behaves once sessions are running, and how to steer it. Start with
[`README.md`](../README.md) if you have not installed it yet. For the exact, normative
behavior — every guarantee, every width rule, every refusal — read
[`docs/SPEC.md`](SPEC.md); this guide is the shorter, friendlier tour and links there
rather than repeating it.

- [How it works](#how-it-works)
- [What flows silently vs. what reaches you](#what-flows-silently-vs-what-reaches-you)
- [The goal (`g`)](#the-goal--what-the-session-works-toward-g)
- [The directive (`i`)](#the-directive--a-standing-never-do-x-for-autopilot-i)
- [Reading the SESSIONS list](#reading-the-sessions-list)
- [Registry](#registry)
- [FAQ](#faq)

## How it works

`pmd` treats each project as **one persistent `claude`/`codex` session** living in a
private-socket tmux server (`tmux -L <socket>`). On its heartbeat the daemon:

1. **watches** the session — is the agent working now, idle, or waiting on you?
2. **nudges** it when it goes idle, via `send-keys`, so it resumes working the goal on
   cadence;
3. **reads decisions** from the agent's marker and from positively identified in-terminal
   choice menus;
4. **routes** each decision. An ordinary one can be auto-handled by a **decider** consult on
   the session's chosen engine (`claude -p` by default, `codex` optional; switch with `e`),
   which checks the concrete choice against the goal, the latest accepted result, and project
   evidence. Anything clearly human-owned — external, privileged, irreversible, or outside
   the goal — **escalates** to the log plus a desktop notification and parks the session for
   you;
5. **detects stalls** — a session that goes quiet without reporting or making real transcript
   progress is surfaced. A healthy long turn with changing output is not a stall.

`pmd`, not the worker, orders reports, so a worker cannot make a later report look stale by
moving its own sequence number. A finished turn that reported nothing gets a corrective nudge
only once the terminal is stably idle.

For a visual walk-through of how Autopilot evaluates and resolves a decision, open the
standalone [Autopilot decision guide](DECIDER-ARCHITECTURE.html). (The
[workbench prototype](PMTUI-WORKBENCH-PROTOTYPE.html) is kept only as history of the browser
exploration that preceded the terminal dashboard; [SPEC](SPEC.md) defines what shipped.)

### Long work that should not hold the terminal

For work that can safely continue unattended, the worker records a handle and its durable
output in a bounded, agent-owned `checkpoint.json`, reports `monitoring`, and lets the
terminal go idle; a later heartbeat inspects the result. That keeps a long wait from looking
like a wedged turn, and carries concise progress, decisions, blockers, and next actions
across context compaction.

The checkpoint never replaces your goal or the required decision marker, and its text is
treated as untrusted display-only continuity data — never as instructions. Workers reload it
only through `pmd checkpoint <path>`, which rejects malformed, oversized, or unsafe data.

### One terminal, optional driver

Standard launches the engine immediately with its normal interactive posture, and `enter`
attaches you to that same terminal. Switching to Autopilot makes `pmd` drive it; switching
back makes `pmd` stop typing — without restarting the process or losing the conversation.

Autopilot is a goal-level supervisor **above** the engine's own permission mode. Claude and
Codex still decide whether a concrete tool call needs permission; `pmd` decides which
goal-aligned work or ordinary choice happens next, and records that decision for review.

Permission, trust, and command-approval prompts are human-only. Codex directory trust is
never auto-approved: its first-run prompt stays for you to answer once, and an unattended
Autopilot session that reaches it is parked and surfaced. Only menus whose own chrome
identifies them as ordinary choices can be delegated, and quoted text is not enough — `pmd`
sends one navigation key and requires the live highlight to move before it will press Enter.

### Reviewing what Autopilot did (`v`)

Press **`v`** on an Autopilot row for its bounded, pmd-owned audit. It opens on **Decisions**
— the configured decider, the in-flight consult, the policy classification, the outcome, the
reason and the duration — and **Tab** switches to **Turns**, which pairs each delivered
heartbeat with the report that followed it, its duration and its next step. Withheld wakes
stay visible, so the view also explains why `pmd` did *not* type. Both tabs record observable
inputs and outcomes, never hidden model reasoning. Scroll with the wheel, `j`/`k`, arrows or
the Page keys.

### Handing work to a new session

Every terminal that pmtui or pmd launches with `PMTUI_BIN` in its environment first gets the
`pmtui-spawn` skill in the project root, so the agent inside knows how to ask for a child
session. The skill is a short procedure: spawn only independent work, write a self-contained
Message, run `pmtui spawn` with one generated request id, and act on the JSON `state` even on
a nonzero exit. Nested spawns are refused and a sixth child waits its turn, and neither
process ever creates or edits your `AGENTS.md`. See the [`pmtui spawn` section in the
README](../README.md#handing-work-to-a-child-session-pmtui-spawn) for the command itself.

### What the dashboard said earlier

The bottom bar shows the latest status line only. Everything it has ever said is appended to
`pmtui.log`, next to your registry — `~/.config/pmd/pmtui.log` by default, and beside a
`--registry` you pass explicitly. Press `?` to see the exact path.

```bash
tail -f ~/.config/pmd/pmtui.log
```

Each line is timestamped in UTC and the file survives restarts, so a failure you missed while
looking at another session is still there. At 1 MiB it rotates — `pmtui.log` becomes
`pmtui.log.1`, `.1` becomes `.2`, and anything older is deleted — so you keep roughly the last
three megabytes of history and never have to clean anything up. Writing it is best effort: if
the file cannot be written, pmtui carries on without it.

### Writing prose: the message (`s`), the goal (`g`) and the directive (`i`)

These three fields are real editors, not one-line boxes, because each is prose an agent or the decider
will act on. They use [`ratatui-textarea`](https://github.com/ratatui/ratatui-textarea), so the keys are
the ones your shell already gives you:

| | |
|---|---|
| `Ctrl+J` (or `Alt+Enter`) | new line — **`Enter` submits**, so a newline needs a chord |
| `Alt+b` / `Alt+f` | back / forward a word |
| `Ctrl+W`, `Alt+Backspace` | delete the word behind the caret |
| `Alt+d` | delete the word ahead |
| `Ctrl+A` / `Ctrl+E` | start / end of the line |
| `Ctrl+K` | delete to the end of the line |
| `Ctrl+U` / `Ctrl+R` | undo / redo |
| `Ctrl+X Ctrl+E` | open the buffer in `$EDITOR` — bash's own chord |
| `Esc` | close (the message keeps its draft; the goal and directive discard the edit) |

`Shift+Enter` also opens a line *if* your terminal reports it as different from `Enter` — many do not
(tmux included), which is why `Ctrl+J` is the one to reach for.

A pasted snippet keeps its line breaks, and a multi-line message arrives at the agent as **one**
input rather than being submitted line by line.

The goal and directive fields **open on what is already in the file**, every line of it, so editing an
existing mandate is typing in it rather than retyping it. They used to open empty above a read-only
preview, because a one-line field could not show a multi-line file. Two notes specific to them:

- An **empty save still keeps** the current text — the same rule `$EDITOR` has always had, so clearing
  the buffer and pressing `enter` is not how you blank a goal.
- **`Ctrl+X Ctrl+R` rescinds** a directive (it was bare `^X` before `^X` became the editor prefix).
  That is the deliberate way to remove one.

The cadence, rename and switcher fields stay single-line — an interval, an id and a query are not
prose — and so do the create form's rows, where `tab` moves between fields and `^E` still opens
`$EDITOR`.

### Typing a project directory

Focus the create form's **Directory** row and it lists the directories you can go to, right away —
no typing needed, since the form opens on your working directory. Each candidate shows the **whole
path** it would put in the field, so there is nothing to work out in your head.

Two ways to use it:

- **Pick one.** `↑`/`↓` move through the list and **`enter`** takes the highlighted path. Since the
  taken path ends in `/`, the list immediately shows what is inside it — so `↓ enter ↓ enter` walks
  down a tree. While a candidate is picked, `enter` takes the path rather than creating the session,
  and `esc` backs out of the list and leaves the form alone; a second `esc` cancels the form. With
  nothing picked, `enter` creates it as always.
- **Type it.** The list narrows as you type, and the rest of the one path your text still allows
  appears dimmed after the caret: **`→`** takes that. A unique match comes with its trailing `/`, so
  the next `→` descends a level.

**`tab` always means "next field"** on every row of this form, whatever the list is offering — so
you can never get stuck here. `shift+tab` goes back. Taking something is always a key that means
taking: `enter` for a candidate, `→` for the completion. The keybar on the bottom border names the
keys that are live.

It offers directories only, keeps dotfiles out of the way until you type the dot yourself, and `~/`
works. When there is nothing picked and nothing to complete — an ambiguous stem, a path that does
not exist, the caret parked mid-path, or a path that already names a real directory — `tab` is the
plain "next field" it always was, so tabbing through the form never gets stuck on this row. While
that row has the arrows, **`shift+tab`** is how you go back a field. To descend from a path you
typed in full, type the `/` and its children become candidates.

### Codex sessions and their conversation

Claude lets pmtui name a conversation up front; Codex does not — it assigns its own. So a Codex session
learns its identity from the engine itself, reported when its **first turn finishes**. Until then the
session has no conversation to go back to, which is why a brand-new Codex row shows none.

Once it has one, `Enter` resumes that conversation and a restart keeps it. Before this, neither the
dashboard nor the driver had any way to know the id, so a Codex session that had been working for hours
still looked brand-new: pressing `Enter` opened a *second* conversation and the first became unreachable,
and every driver relaunch started over. If you have Codex rows from an older version, their pre-existing
conversations cannot be recovered — but from the first completed turn onward each one is remembered.

### Jobs an agent spawned

An agent working in a session can hand an independent piece of its task to a child. That child is a
**job**: one headless agent run that does the task, reports back, and exits. While it runs you see a
row like any other, titled `<id> · job`, and the keys that assume someone is listening (`s`, `m`,
restart, fork) refuse it, because nobody is.

Its preview is **rendered, not captured**. A job's own pane is a stream of machine-readable events, so
the preview reads the log that stream is tee'd to and shows what the agent said, the tools it ran, and
how it ended — the same shape a wake transcript has. `enter` still attaches you to the raw pane if you
want it.

When it finishes, the dashboard cleans up after it: a job that succeeded has its terminal closed and
its row removed, with one line in the status log naming what it did. A job that **failed**, that needs
a person, or whose run died without reporting KEEPS its row, paused, so you see it — press `d` to
clear it, which takes its files with it unless it left work it never committed. A job's state lives exactly as long as its row: a
retired one leaves nothing under `.project-state/`, because what it achieved is in the answer its parent
reads on the next check. A row that is kept keeps its log, which is the thing you still have to read.

Nothing times a job out, and nothing has to guess how long one will take: the agent dispatches, gets on
with its own work, and asks `pmtui spawn --status` what became of all its children when it next looks up.
Several children run at the same time — each in its own terminal, each answered on its own — as long as
the agent dispatches them all before it waits on any of them. Five run at once; ask for more and they wait
their turn rather than being refused.

In a git repository each child also gets **its own checkout**: a worktree on a branch of its own —
`pm/<id>-<first 8 of the request id>` — so two children editing the same file never see each other's
edits. The child is told
to commit there, and nothing of its work reaches your checkout until you say so — press **`a`** on a
finished job's row to cherry-pick its commit onto the branch you are on. That asks first, refuses while
your own checkout has uncommitted changes to tracked files (untracked ones are fine — `.project-state/` is
one), and on a conflict puts everything back and tells you. A job that
left work it never committed keeps its row and its worktree: those changes exist nowhere else, so nothing
deletes them to tidy up.

You can stop one at any point with `d`. The agent can stop its own child too, and then you see the
dashboard do it: the status line says `cancelling <id> — asked it to stop`, and a moment later
`retired <id> · cancelled`.
It asks the run to finish before it kills the terminal, so the child gets to clean up after itself. A
child that had already reported keeps what it reported — a cancel that arrives late does not erase
finished work.

### Picking a theme (`0`)

`0` opens **Settings**, the third tab after Sessions and Tasks. It is a table: one row per
setting, with its name, what it is set to, and what it governs.

```
  Setting       Value                     Enter opens the list
  Theme         Catppuccin Mocha        ▾ every colour the dashboard draws
```

**Enter** on a row drops its list of values — every theme
[opaline](https://github.com/hyperb1iss/opaline) ships, by name, with `dark` or `light` beside it.
The list opens on the value in force, marked `●`; `j`/`k` move, **Enter** applies the one under the
cursor and writes it down, **Esc** closes the list and changes nothing. Nothing changes until that
second Enter, so you can walk the whole list without the screen repainting under you. The mouse
works the same way: click a row to open its list, click a value to pick it.

Choosing a light theme in a dark terminal is the one mistake worth watching for, which is why the
`dark`/`light` column is there.

The choice lives in `pmtui.json` beside your registry (`~/.config/pmd/pmtui.json` by default; the
Settings view names the exact path at the bottom of its frame):

```json
{ "theme": "catppuccin-mocha" }
```

A scratch `--registry` gets its own preferences, so a throwaway dashboard cannot change how your
real one looks. If you hand-edit the file and name a theme that does not exist, pmtui starts on the
default and the status line says which id it refused.

The theme paints everything: background, borders, and all the text, including the plain text that
has no colour of its own. pmtui fills the whole frame with the theme's background rather than letting
your terminal's show through, and an agent's transcript keeps the colours the agent chose while its
"default colour" becomes the theme's. If you keep a translucent terminal and would rather see
through it, that is the trade — a light theme is unreadable without it.

### When something looks wrong

Run `pmd doctor`. It checks the pmd version, registry identity, project roots, per-session
config and state, worker and spawn-skill freshness, required executables, notification
availability, worker markers, orphaned state directories, and the tmux socket (including
whether tmux is new enough for managed launches). Doctor never starts, stops, repairs, or
types into a session. Results are `PASS`, `WARN`, `FAIL`, or `SKIP`; warnings exit `0`,
failures exit `1`. Add `--json` for the versioned report.

Everything is file-based and single-writer: `pmd` is the only writer of a session's ledger,
and `pmtui` acts through the registry and the human-owned config, control, answer, goal and
directive files. That is why restarting either process simply rebuilds the world from disk.

Only one `pmtui` may mutate a registry/socket pair. Starting a second one in an interactive
terminal offers a confirmed takeover: it asks the running dashboard to exit, waits for the
lock, then starts the replacement. A takeover never stops `pmd` or any project terminal.

## What flows silently vs. what reaches you

Two inputs decide whether a decision reaches you:

- **Per-project tier** — the autonomy dial, and the on/off switch for the whole harness:

  | Tier | `pmd` drives it? | Escalates | Use it when |
  |---|---|---|---|
  | `autopilot` | **yes** — nudges on cadence | explicit human-owned decisions | you want hands-off progress |
  | `standard` (default) | **no** — you drive by hand (`enter`) | medium **and** hard | you want to stay in the loop |

  Turning autopilot off does **not** kill a running session — you just take the wheel.

- **Per-stop risk class** the agent sets on each decision it raises: `low`, `medium`, `hard`.

Judgment starts with the agent's own report, and the daemon re-checks the typed kind and
effect classification. Explicit external, irreversible, or privileged effects always stay
human-owned. Ordinary decisions — including partly classified ones — go to the read-only
decider, which weighs the concrete action against the goal and project evidence. Each
decision gets its own validated verdict, and co-reported decisions are reviewed one at a
time. If the decider is disabled, unavailable, interrupted, or refuses, that decision reaches
you. The normative rules are in
[SPEC §6, Decision authority and safety](SPEC.md#6-decision-authority-and-safety).

## The goal — what the session works toward (`g`)

The **goal** (the session's *brief*, `brief.md`) is its single source of direction: what is
this session doing, and what does *done* look like? On every heartbeat `pmd` injects it
**verbatim** under a `## Your goal` heading, so the agent re-reads the whole brief each time
it is nudged, and the decider sees it as grounding for the calls it makes.

A hands-off loop with no direction is a nudge with nothing to steer by, so **autopilot needs
a goal**: the create form asks for one when you pick `autopilot`, and `g` on a `standard` row
refuses and points you at `m` — on `standard` *you* are the steering.

Write it like a **brief to a capable engineer**, not a one-word label: the outcome, the key
constraints, and any context the agent cannot infer. Press **`Ctrl+E`** to compose it in
`$EDITOR`. **Enter** saves, an **empty** save keeps the current goal, and it takes effect on
the next wake — no restart.

```text
Migrate the auth service from session cookies to short-lived JWTs.
- Keep /login and /logout working unchanged; no breaking public-API changes (add, don't replace).
- Every change behind tests; keep the suite green.
Done = JWT path shipped behind a flag, old path still passing, migration notes in docs/auth.md.
```

Weak goals leave the agent guessing on every wake: `fix auth` (fix what, to what end, within
what limits?) and `make it better` (no outcome, so each wake re-invents the task).

### Goal vs. directive

They are a pair, and easy to mix up:

| | Key | Says | Shape | Read when |
|---|---|---|---|---|
| **Goal** | `g` | what to **do** — the outcome to pursue | positive, open-ended | every wake, and by the decider |
| **Directive** | `i` | what to **never auto-do** | negative, restrictive-only | on every decider decision |

Set the goal so the agent knows where to go; add a directive when there is a line it must not
cross getting there.

## The directive — a standing "never do X" for autopilot (`i`)

On an **autopilot** session, an ordinary decision goes to the decider consult. The
**directive** is your standing, per-session rule that makes that consult **more cautious**.
It is read fresh on *every* decision, so it steers all of them, not just the next one.

It is **restrictive-only**, and this is the whole point: a directive can only ever **forbid**,
never permit. If the decision in front of the decider would violate it, the decider refuses
and hands that decision to you. So a directive can turn a would-be auto-approval into an
escalation; it can never turn an escalation into an auto-approval.

**It only bites on autopilot**, because the decider only runs while `pmd` drives the row —
`i` on a `standard` row refuses and points you at `m`. And you do not need it for clearly
human-owned effects: those
[always escalate](#what-flows-silently-vs-what-reaches-you) regardless of tier or directive.
Use it for *your project's own* "don't auto-do that" rules, the ones no generic effect
classification knows about.

To set one: select the session, turn on autopilot (`m`), press **`i`**, and type the rule
inline or **`Ctrl+E`** for a longer one. **Enter** saves, an **empty** save keeps the current
directive, and **`Ctrl+X`** rescinds it. It applies to the next decision — no restart. Keep
it short: past about a kilobyte the decider is shown a truncated copy.

Phrase every rule as a prohibition:

```text
Never auto-approve edits under migrations/ or db/schema/ — always ask me first.
Don't auto-approve deleting or overwriting a file; ask before any destructive change.
Never auto-approve anything that touches production config or runs the deploy script.
Don't choose between competing library/API options on your own — those calls are mine.
```

`Always auto-approve test-file edits` will **not** do what it looks like: a directive cannot
grant permission, so it is ignored as authorization and the decider still weighs each case.

## Reading the SESSIONS list

Rows are grouped by **what they need from you**, then by **who drives them**, because that is
what decides your next move:

```
  NEEDS YOU (1) · action needed
  ◐ asking           needs you     [A]  3m
  AUTOPILOT (1) · pmd drives
  ● driven           working       [A] 12m
  STANDARD (1) · you drive
▸ ○ mine             monitoring    [S]  1h
  PAUSED / OFFLINE (2) · not running
  ○ stopped          paused        [S]  2h
  ○ exited           offline       [S]  5m
```

Unless it is paused, a session waiting on you is listed under NEEDS YOU whatever its mode. A
session whose terminal is gone and that is not waiting on you shows as `offline` under
PAUSED / OFFLINE, keeping its `[A]`/`[S]` tag.

Per row: a state glyph, the session id, what it is doing, the tier, and how long since it
last did anything. **The glyph means activity, not liveness:** `●` = the agent is working
*now*, `○` = idle or finished. A row that wants you is filled — **yellow** if the decision is
reversible, **red** if it is not.

## Registry

`pmd` reads a registry JSON (default `~/.config/pmd/registry.json`; override with
`--registry`). It is reloaded each sweep, so pausing or adding a project takes effect live.

```json
{
  "projects": [
    {
      "id": "auth-rewrite",
      "root": "/path/to/your/project",
      "enabled": true,
      "engine": "claude"
    }
  ]
}
```

pmtui's `n` create form writes these entries for you. Per-project autonomy (the tier) and the
goal live in the project's own per-session state, not here.

## FAQ

<details>
<summary><b>Does it work without the daemon running?</b></summary>

`pmtui` does — it is file-based, so it always renders and can pause, remove, retier, or edit
a goal. But nothing gets *driven* unless `pmd` is running: `pmd` is what nudges autopilot
sessions and reads their markers.
</details>

<details>
<summary><b>What does "autopilot" actually change?</b></summary>

It is the on/off switch for the harness on that project. `autopilot` ⇒ `pmd` nudges the live
agent on its cadence and auto-handles low-stakes decisions. `standard` ⇒ `pmd` does not touch
it at all; you drive by hand. See
[What flows silently vs. what reaches you](#what-flows-silently-vs-what-reaches-you).
</details>

<details>
<summary><b>What engine and model does the decider use? (<code>e</code>)</b></summary>

**Claude** by default. On an **autopilot** row, press **`e`** to pick that session's decider
engine and model in two steps: the engine (`claude` or `codex`), then **Enter** for its model
list (`(default)` plus the models discovered for that engine). ↑↓ move, Enter selects, Esc
steps back or cancels. It writes only `config.json`, so it applies to the next decision with
no restart, and `(default)` falls back to the engine's own default. Whatever you pick, a
reply that is not a clean verdict escalates to you. (`e` on a `standard` row refuses and
points you at `m`.)
</details>

<details>
<summary><b>How do I set the model the agent (worker) runs on? (<code>w</code>)</b></summary>

Two ways. In the **create form**, the **Worker Model** field is a ←→ stepper over
`(default)` plus the engine's discovered models, with the catalog shown inline under the
field and the current value marked `●`. For an existing session, select its row and press
**`w`** to open the same picker. The worker model is baked into the agent's launch command,
so a change is saved to the registry and the dashboard asks you to press **`r`** to restart
the agent to apply it — it does not hot-swap a running REPL. `(default)` clears it. The
worker *engine* stays create-time only.
</details>

<details>
<summary><b>Where do the model lists come from?</b></summary>

Discovered from the installed CLIs, best-effort. **claude** uses `~/.claude/settings.json`
when it has an explicit `availableModels` allowlist, resolving entries through
`modelOverrides`; on a direct Anthropic setup with no allowlist the picker offers the stable
`sonnet`, `opus` and `haiku` aliases (Claude still enforces what your subscription may use).
Bedrock, Vertex, Foundry, Mantle, Anthropic AWS and custom-base-URL setups need an explicit
allowlist, because a bare alias may not select the provider model you meant. **Codex** models
come from `codex debug models`. The list is cached per engine for the dashboard's lifetime —
restart `pmtui` to refresh it.
</details>

<details>
<summary><b>Do I need Rust installed?</b></summary>

Only if there is no prebuilt binary for your platform yet — then the installer builds from
source and needs `cargo` (see [rustup.rs](https://rustup.rs)). Prebuilt installs need only
`curl`/`tar`, plus `tmux` at runtime.
</details>

<details>
<summary><b>How do I turn off auto-update?</b></summary>

Set `AGENT_MANAGER_NO_UPDATE=1` in your environment, or install with `--no-wrapper` to skip
the launcher wrappers entirely.
</details>

<details>
<summary><b>What else can the installer do?</b></summary>

```bash
# always build from source with cargo
curl -fsSL https://raw.githubusercontent.com/hung-phan/pmtui/main/install.sh | bash -s -- --from-source

# a custom bin dir, a specific release tag, or the raw binaries with no update wrapper
... | bash -s -- --dir /usr/local/bin
... | bash -s -- --version v0.1.0
... | bash -s -- --no-wrapper

# update now, or remove everything (your registry and project state are left alone)
... | bash -s -- --update
... | bash -s -- --uninstall
```

Run `install.sh --help` for the full flag list.
</details>

<details>
<summary><b>How do I see UI changes without touching my real sessions?</b></summary>

```bash
scripts/pmtui-dogfood.sh                    # 80x24, 100x28 and 120x32
scripts/pmtui-dogfood.sh --size 100x28      # one exact viewport
```

It writes plain and color-preserving captures under `target/pmtui-ui/` and never uses the
live registry, the pmd socket, or real agent executables.
</details>
