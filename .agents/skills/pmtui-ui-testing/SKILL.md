---
name: pmtui-ui-testing
description: How to render and drive the pmtui dashboard under tmux to test the UI end-to-end — launch it headless on a scratch tmux server + scratch registry, navigate with send-keys, read the screen back with capture-pane, and exercise specific render states by seeding .project-state/ files. Invoke whenever you change anything the dashboard draws (src/bin/pmtui/, the row/preview/help/chrome render code, glyphs, keybar) and want to SEE the result instead of trusting a unit test, or when the user asks you to render/screenshot/verify the TUI.
---

# Testing the pmtui UI with tmux

pmtui is a full-screen ratatui app: it takes over the terminal, so you cannot run it
directly and read its output. Instead you run it **inside a tmux pane you own**, drive it
with `send-keys`, and read the rendered screen back with `capture-pane`. pmtui is
file-based — it renders from a registry + each project's `.project-state/`, and needs no
running `pmd` — so a pre-seeded scratch registry gives you any row state you want with
nothing else running.

## Fast path: laptop UI gallery

For routine visual review, use the checked-in gallery harness before assembling a
one-off fixture:

```bash
scripts/pmtui-dogfood.sh
scripts/pmtui-dogfood.sh --size 100x28
```

It drives the real integration-test `pmtui` over private tmux sockets and writes
plain plus ANSI captures under `target/pmtui-ui/<UTC timestamp>/`. The default
matrix represents a constrained terminal (`80x24`), the primary 13-inch laptop
target (`100x28`), and a maximized laptop terminal (`120x32`). It captures the
main dashboard, dense Task columns, Task inspector/create/Answer states, Needs
You answer surface, long Message composer, switcher, and help. Use
`--sizes 63x24,64x24,89x28,90x28,97x28,98x28` when reviewing responsive
boundaries.

The gallery stages `pmtui` without a sibling `pmd`, uses typed scratch state,
launches only inert shell panes, and tears down both unique tmux servers through
the integration fixture. It cannot reach the live registry or socket.

Work through the steps in order. Everything lives on scratch paths so a test run cannot
touch the user's real projects or their real `pmd` tmux server.

## Safety — read before the first `tmux` call

- **Never pass `--socket pmd`.** `pmd` is the user's real private tmux server; a test that
  attaches to it can kill or disturb live agent panes. Always use a scratch socket name
  (`pmtuitest-inner` below).
- **Never point `--registry` at `~/.config/pmd/registry.json`.** That is the user's real
  project list. Always use the scratch `registry.json` you write under `$SCRATCH`.
- **Two tmux servers, both scratch.** `tmux -L pmtui-test` HOSTS the pmtui process so you
  can drive it. `--socket pmtuitest-inner` is the private server pmtui would launch agent
  panes on. Kill both in cleanup.
- **Pre-seeded idle rows launch nothing.** A render test with `mode: "agent_loop"` rows and
  no `pmd` shows the rows without starting any agent. Pressing `n` (create) or `enter`
  (attach) DOES launch a real `claude`/`codex` REPL — only do that when you mean to test
  that path, and clean up the REPLs afterward.

## 1. Build the REAL binary

```bash
cargo build          # produces target/debug/pmtui and target/debug/pmd
```

Do this after every code change you want to see. `cargo test --bins` does NOT update
`target/debug/pmtui` — it builds a separate test-harness binary and leaves the real one
STALE, so you would render your OLD code and not know it. If a fix "doesn't show up",
suspect a stale binary first: rebuild with `cargo build` and re-render.

## 2. Lay down a scratch registry

```bash
SCRATCH=$(mktemp -d)
BIN="$PWD/target/debug/pmtui"          # run this from the repo root
cat > "$SCRATCH/registry.json" <<JSON
{
  "projects": [
    { "id": "demo-1", "root": "$SCRATCH", "enabled": true, "coordinator_cmd": [],
      "mode": "agent_loop", "engine": "claude", "initial_prompt": null,
      "conversation_id": null, "cadence_s": null },
    { "id": "demo-2", "root": "$SCRATCH", "enabled": true, "coordinator_cmd": [],
      "mode": "agent_loop", "engine": "claude", "initial_prompt": null,
      "conversation_id": null, "cadence_s": null }
  ]
}
JSON
```

With no `.project-state/config.json` the tier is unknown (the row shows `[?]` and
`autopilot ?`), and with no terminal running it groups under `PAUSED / OFFLINE (n) · not
running` as an `offline` row — enough to test the head line, the help overlay, row grouping,
glyphs, and chrome. Seed `config.json` (§6) to render a concrete `standard`/`autopilot`
tier, or to exercise driven/attention states.

## 3. Launch pmtui on a scratch tmux server

```bash
tmux -L pmtui-test kill-server 2>/dev/null; sleep 0.3
tmux -L pmtui-test new-session -d -s ui -x 140 -y 42 \
  "cd '$SCRATCH' && ECC_GATEGUARD=off '$BIN' --registry '$SCRATCH/registry.json' --socket pmtuitest-inner"
sleep 2      # let the first frame render
```

- `-x`/`-y` fix the pane size, so the layout is reproducible. Test more than one: `140x42`
  (wide) shows everything; a narrow size like `100x24` exercises the responsive shedding
  (the age and `chat` chip drop when the pane is too narrow).
- `ECC_GATEGUARD=off` stops the GateGuard hook from stalling any child pmtui might spawn.

## 4. Read the screen back

```bash
# Plain text — what the layout looks like:
tmux -L pmtui-test capture-pane -p -t ui

# With colors — SGR escapes made visible (verify attention hues, the reversed chip, dim):
tmux -L pmtui-test capture-pane -e -p -t ui | cat -v

# One specific row (e.g. the preview head is around line 3):
tmux -L pmtui-test capture-pane -p -t ui | sed -n '3p'
```

## 5. Navigate with send-keys

Send one key, `sleep ~0.5`, then capture. Keys (from the `?` overlay, the source of truth):

| Key | Does |
|---|---|
| `j` / `k`, `Down`/`Up` | move the selection |
| `Enter` | attach / resume (LAUNCHES a real agent on an interactive/agent-loop row) |
| `?` | key-reference overlay (`Escape` or any key closes it) |
| `s` | Send normally or Answer an open stop · `m` mode · `g` goal · `c` cadence · `i` directive |
| `Escape` | close an overlay · `q` quit pmtui |

```bash
tmux -L pmtui-test send-keys -t ui j        ; sleep 0.5
tmux -L pmtui-test send-keys -t ui '?'      ; sleep 0.6   # open help
tmux -L pmtui-test capture-pane -p -t ui
tmux -L pmtui-test send-keys -t ui Escape   ; sleep 0.3   # close it
```

Send a literal key name (`Enter`, `Escape`, `Up`, `PgDn`) unquoted; quote a bare glyph
like `'?'` so the shell does not expand it.

## 6. Exercise specific render states (optional)

Idle standard rows cover a lot. For driven / attention / report states, seed files under
`$SCRATCH/.project-state/` BEFORE launch (one writer per file — see the README table):

- **Autopilot / driven row:** write `config.json` with the autopilot tier so
  `daemon::pmd_drives_row` reports the row as driven and it shows `[A]`. It groups under
  `AUTOPILOT` only while its terminal is live; a row with no terminal stays under
  `PAUSED / OFFLINE`.
- **An open decision (needs-you badge, filled row, contextual `s` Answer overlay populated):** write
  `stops.json` with a stop carrying a `risk_class`.
- **The agent's `report` section + status chip:** seed the agent-loop ledger / marker
  (`AgentLoopState`, `last_status`) the preview reads.

Match the exact on-disk shapes to the structs in `src/state/`, `src/job.rs`, and
`src/pmstate/`; grep a unit test that builds the same record for a known-good literal.

## 7. Clean up — always

```bash
tmux -L pmtuitest-inner kill-server 2>/dev/null   # any agent panes pmtui launched
tmux -L pmtui-test    kill-server 2>/dev/null      # the pmtui host server
rm -rf "$SCRATCH"
# If you pressed n/enter and spawned REPLs, confirm none linger:
pgrep -af 'claude|codex' | grep "$SCRATCH" || true
```

Kill the scratch servers even if pmtui already exited — a detached `new-session` server
outlives the app. Leaving `pmtui-test` running wastes a socket; leaving `pmtuitest-inner`
running can leave orphaned `claude`/`codex` REPLs.

## Gotchas

- **Stale binary** — the most common wasted render. If output contradicts your edit, `cargo
  build` and re-render before debugging anything else (§1).
- **Capture too early** — a capture before the first frame returns a blank or half-drawn
  screen. `sleep 2` after launch, `sleep ~0.5` after each key.
- **Trailing spaces** — ratatui pads every line to the pane width; pipe through
  `sed -e 's/[[:space:]]*$//'` when you want to compare text.
- **Width matters** — a bug may only appear at one size. Re-run §3–§5 at a narrow width to
  catch truncation and responsive shedding.
