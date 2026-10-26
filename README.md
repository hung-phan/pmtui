<div align="center">

<img src="docs/logo.png" alt="pmtui logo" width="320">

**Keep one AI coding agent per project moving toward its goal — and only pull you in when a decision genuinely needs a human.**

![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20WSL-1a1b26?style=for-the-badge)
![Rust](https://img.shields.io/badge/Rust-edition%202024-1a1b26?style=for-the-badge&logo=rust)
![Runs on](https://img.shields.io/badge/runs%20on-tmux-1a1b26?style=for-the-badge&logo=tmux)
![Status](https://img.shields.io/badge/status-beta-1a1b26?style=for-the-badge)

[Install](#install) · [Quick start](#quick-start) · [Keys](#keys-pmtui) · [Ways to use it](#ways-to-use-it) · [Skills](#what-the-agent-can-do-for-itself-the-skills) · [Guide](docs/GUIDE.md) · [Spec](docs/SPEC.md)

</div>

---

`pmtui` is a native Rust supervisor for long-running `claude` / `codex` sessions. Each project
session owns **one persistent tmux terminal**. It gets nudged on a cadence so it keeps making
progress toward a goal you set, and reaches you **only when a decision genuinely needs a human**.

![The Sessions view: the session rail grouped by who drives each row, with the selected session's live transcript and its open decision beside it](docs/Sessions.png)

**Sessions** — every project grouped by who drives it, with the selected session's live transcript
beside it. The rail tells you where you are needed; the pane tells you why.

![The Tasks view: every session as a card in the Paused, Needs You, Pending, Autopilot and Working columns](docs/Tasks.png)

**Tasks** — the same sessions as cards, in the columns that answer "what should I do next".

Two binaries:

- **`pmd`** — an optional headless driver. For each enabled project it runs the heartbeat: nudge
  the agent when it goes idle, read the decision marker it writes, detect stalls, and route
  escalations to the log and desktop notifications.
- **`pmtui`** — a ratatui dashboard over those projects. It is **file-based**: it reads each
  project's on-disk state to render and writes files to act, so it works whether or not the
  daemon is running.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/hung-phan/pmtui/main/install.sh | bash
```

That puts `pmd` and `pmtui` in `~/.local/bin` — a prebuilt, checksum-verified binary for your
platform when one is published, otherwise a `cargo` build from source. The Linux binaries are
statically linked, so they do not care which distribution or libc version you run; and the
installer starts each downloaded binary before installing anything, falling back to a source build
if it will not run on your machine. **tmux** is required at
runtime and managed launches need **tmux 3.0 or newer**; the installer tells you how to get it, and
`pmd doctor` reports a tmux that is too old. If `~/.local/bin` is not on your `PATH`, the installer
prints the one line to add.

Installed commands are thin wrappers that check for updates in the background at most once a day
and run the real binary immediately, so opening is never blocked. You can also ask from inside the
dashboard, on the **Updates** row of Settings. Updating, uninstalling, pinning a version and
source-only builds are all flags on the same script — see
[the installer FAQ](docs/GUIDE.md#faq).

## Quick start

```bash
pmtui          # open the dashboard
pmd            # run the driver (optional; it is what drives autopilot rows)
pmd doctor     # inspect setup and state, changing nothing
```

Both read `~/.config/pmd/registry.json` and the private tmux socket `pmd` by default; `--registry`
and `--socket` override them, and `--help` lists the rest.

In the dashboard:

1. **`n`** creates a session. Point it at a project directory — the **Directory** row lists where
   you can go as whole paths (`↑`/`↓` to pick, `enter` to take) and completes as you type — and
   optionally type a first **Message**: the interactive Claude/Codex terminal starts on it right away. Leave it empty to
   start at the agent's prompt. The **Name** field is an optional human-readable label.
2. **`enter`** attaches you to that session's terminal. It is an ordinary `claude`/`codex` REPL:
   work in it, press **`ctrl+q`** to leave it, and the session keeps its conversation — the agent
   keeps running, and you are back on the dashboard.
3. **`m`** turns on **autopilot**. From then on a running `pmd` nudges that session on its
   cadence, auto-handles ordinary choices, and escalates the rest to you. Give an autopilot
   session a goal with **`g`** — it is the direction every nudge steers by.
4. **`1`**, **`2`** and **`0`** switch views: **Sessions**, the terminal-first list with the selected
   session's transcript beside it, **Tasks**, the same sessions as cards in Paused, Needs You,
   Pending, Autopilot and Working columns, and **Settings**, a table of preferences — the theme,
   from the 38 `opaline` ships, and **Updates**, which says which pmtui you are running and, in one
   keypress, installs a newer one and restarts onto it. The active view is the underlined tab in
   the header.
5. **`s`** sends one message without taking over the terminal, or answers an open decision.
   **`?`** lists every key.

The mouse works wherever the keyboard does: click a row or card to select it, the preview title to
attach, or any visible keybar chip to run it.

Next: [**docs/GUIDE.md**](docs/GUIDE.md) walks through autopilot, goals, directives and the
session list.

### Keys (`pmtui`)

| Key | Action | Key | Action |
|---|---|---|---|
| `j` / `k` | move selection | `/` | find and switch session |
| `1` / `2` / `0` | select the Session, Task or Settings view | `n` | new session |
| `enter` | attach (or **resume** a paused row) | `s` | message, or answer an open stop |
| `ctrl+q` | leave an attached session, back to the dashboard (the agent keeps running) | | |
| in the message, goal and directive fields: `^J` new line (`alt+enter` too), `alt+b`/`alt+f` word, `^w` kill word, `^x^e` $EDITOR | | | |
| `f` | fork conversation into a new Standard session | | |
| `R` | rename the selected session | | |
| `m` | mode — the autonomy tier (`standard` ⇄ `autopilot`) | `g` | edit the goal |
| `d` | delete session | `p` | pause |
| `c` | cadence | `r` | restart the agent |
| `i` | edit the standing directive (autopilot) | `e` | pick the decider **engine + model** (autopilot) |
| `q` | quit | `v` | turn and decision audit (autopilot) |
| `w` | set the **worker model** for the selected session (restart to apply) | `?` | key reference |

## Ways to use it

Four shapes, in the order people grow into them. They are the same product with different dials —
nothing to install or configure to move between them.

**1. One agent, and you watching.** The default after `n`: a Standard session. No daemon, no
cadence, nothing nudging it. `enter` attaches, `ctrl+q` leaves it running, and `s` says one thing
without taking the terminal over. Right for a long task you would rather steer than babysit.

**2. One agent, unattended.** `m` for autopilot, then `g` for the goal. A running `pmd` nudges it on
its cadence, the decider answers routine choices *from that goal*, and only what needs you reaches
you. Two levers, and they are not the same: the **goal** (`g`) is where it steers, the **directive**
(`i`) is your standing "never do X" that applies on every wake.

**3. Several projects at once.** The Sessions rail is ordered by who needs you, and Tasks (`2`) is
the same sessions as columns that answer "what next". The point is to stop polling: leave the
dashboard open and let it tell you. `/` jumps straight to a session, `v` shows what autopilot did
while you were elsewhere.

**4. One agent that fans work out.** Give one session a goal whose parts are independent, and it
hands those parts to child jobs itself — that is the `pmtui-spawn` skill, and for a queue that
outlives one wake, `agent-manager-coordinator` ([both below](#what-the-agent-can-do-for-itself-the-skills)).
You do not dispatch anything; children work in their own worktrees and you integrate with `a`.

### Getting more out of it

- **Cadence is your latency *and* your spend** (`c`). A session waiting on a slow build wants
  minutes; a tight edit loop wants a couple; an idle one wants half an hour. An agent can also
  propose its own from its report, so it is allowed to learn the work's real rhythm.
- **Put your effort into the goal, not into nudging.** Every nudge steers by it, so a vague goal
  costs you on every wake. A sharp one is the cheapest thing you can do.
- **The directive is for rules, not tasks.** "Never touch production data" belongs there; "fix the
  parser" belongs in the goal.
- **Don't attach just to find out what happened.** `v` is the turn and decision audit, and the
  preview already shows the live transcript.
- **Pay for the big model where it matters** (`w`, per session) and pick the decider's engine and
  model separately (`e`) — the thing answering routine choices need not be the thing writing code.
- **A child job cannot ask a question.** So when a session fans work out, the sharpness of your goal
  is what the children inherit — a vague one becomes several vaguely-specified jobs at once.

## What the agent can do for itself (the skills)

The three sections below are **not commands you type**. Every managed terminal is installed with
these skills and the environment they need, so the agent in your session reaches for them on its own
when its work has that shape. You read them to know what your sessions are capable of — and which
keys stay yours.

### Handing work out: the `pmtui-spawn` skill

An agent can hand an independent piece of work to a **child job**: one headless run that does the
task, reports a result and exits. You ask for the outcome; it decides a child is the way to get there.

Five children run at once and a sixth waits its turn. In a git repository each one works in its own
worktree on its own branch and commits there, so children never overwrite each other — and nothing
reaches your checkout until **you** press `a` on the finished row. A finished `done` child retires
itself unless it left uncommitted work.

Two consequences for you: **a job cannot ask a question**, so the quality of the spec your agent
writes is the whole limit on what comes back; and a child's row is a *result*, not a merge. The
dashboard shows every child beside its parent, so `d` still removes one if you want it gone.

Under the hood it writes a request into its own state directory and waits for the dashboard to
answer with a receipt — no registry, no tmux, no `$HOME`, so it works inside Codex's sandbox and the
same request id never creates a second child. `pmtui spawn --help` and
[SPEC §4](docs/SPEC.md#4-features) have the contract if you want to drive it by hand.

### Talking to a peer: the `pmtui-message` skill

A child job cannot answer anybody. A **peer** can: an agent in one session can send one message to
another session's agent — to ask what it owns, hand over a finding, or answer something it was asked.

It arrives as **one ordinary user turn** in the other terminal, wearing an envelope that names the
sender, so a reply is just a message back: no mailbox, no polling, nothing blocking. It goes through
the same gate as your own `s`, which is the part that matters to you — **it will not type over you
while you are attached**, will not answer a dialog, and will not interleave with a nudge. A peer
carries no authority: the skill tells recipients to weigh an ask against their own goal and refuse
what conflicts with it. Six messages to one peer in ten minutes is the bound, because two agents
being polite to each other runs forever with nobody watching.

`pmtui message --list` is the one form worth running yourself — it prints
`id (name)  engine  folder  — state` and tells you which sessions can take a message right now.
Address a session by its **id**: two can share a display name and two can share a folder, so neither
identifies one.

### Running a queue: the `agent-manager-coordinator` skill

When a goal is bigger than one session can do in sequence and its parts can proceed without you, a
session runs a **queue** instead of a single task. Nothing to turn on — the skill applies when the
work has that shape.

Each wake it harvests its children in one call, decides what each finished one means, prunes, fills
the free slots, and reports. The queue lives in that session's own continuity checkpoint, so it
survives compaction and restart.

Two things are worth knowing as the human:

- **Its cadence (`c`) is its latency.** Children are harvested when it wakes, so a coordinator with
  work in flight wants a short cadence and an idle one wants a long one. It can propose its own.
- **You are still the merge queue.** Children commit on their own branches and nothing reaches your
  checkout until you press `a`. A coordinator says a branch is ready; it never merges for you, and no
  machine says the project is finished.

The skills divide the work and none repeats another: `agent-manager-worker` is the heartbeat,
`pmtui-spawn` is one dispatch, `pmtui-message` is one peer, and `agent-manager-coordinator` is the
loop around them.

## Learn more

- [**docs/GUIDE.md**](docs/GUIDE.md) — using it day to day: how the heartbeat works, what escalates
  and what does not, goals, directives, the session list, the registry, and the FAQ.
- [**docs/SPEC.md**](docs/SPEC.md) — the living specification: exact, normative behavior and
  guarantees.
- [**AGENTS.md**](AGENTS.md) — contributing: invariants, source layout, and the full test matrix.

## Build & test (contributing)

Requires a recent stable Rust (edition 2024). The coverage gate also needs `tmux`, `jq`, and
`cargo-llvm-cov`. Project-local skills are canonical under `.agents/skills/`; the `.claude/skills/`
entries are symlinks to them, so update only the `.agents` copy.

`scripts/link-release.sh` builds both binaries and links them into `~/.local/bin` so the commands
on your `PATH` are your working copy (`--no-build` refreshes only the symlinks), and
`scripts/pmtui-dogfood.sh` renders the dashboard against isolated scratch sessions.

```bash
cargo build                     # builds pmd and pmtui
cargo test                      # lib + bins + integration + doc-tests (the FULL non-ignored suite)
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check      # must be clean
cargo audit
bash tests/coverage_test.sh
bash tests/install_sh_test.sh
bash tests/repo_contract_test.sh
cargo test --test integration -- --ignored --test-threads=1
```

Some integration tests shell out to **real tmux** and are `#[ignore]`d so `cargo test` stays fast;
that real-tmux acceptance is the load-bearing end-to-end gate. [`AGENTS.md`](AGENTS.md) has the
full test and eval matrix (real-tmux acceptance, the LLM nudge judge, and the decider benchmark).
The decider benchmark runs **both** engines by default; set `PM_DECIDER_BENCH_ENGINE=claude` or
`=codex` to run just one. The original design write-up is
[`docs/superpowers/specs/2026-08-12-agent-manager-daemon-design.md`](docs/superpowers/specs/2026-08-12-agent-manager-daemon-design.md).

## Star history

If `pmtui` saves you time, a star helps other people find it.

<a href="https://star-history.com/#hung-phan/pmtui&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=hung-phan/pmtui&type=Date&theme=dark">
    <img alt="Star history for hung-phan/pmtui" src="https://api.star-history.com/svg?repos=hung-phan/pmtui&type=Date">
  </picture>
</a>

## License

[MIT](LICENSE) © 2026 Hung Phan.

Use it, change it, ship it, sell it — just keep the copyright notice and the license text with it. The
software comes with no warranty.
