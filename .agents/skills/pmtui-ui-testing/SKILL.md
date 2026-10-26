---
name: pmtui-ui-testing
description: How to render and drive the pmtui dashboard under tmux, to test the UI end to end. Launch it headless on a scratch tmux server and a scratch registry. Navigate with send-keys, and read the screen back with capture-pane. Set up a render state by seeding .project-state/ files. Invoke whenever you change what the dashboard draws. That includes src/bin/pmtui/, the render code for a row, preview, help or chrome, the glyphs and the keybar. Invoke it also when you want to SEE a result instead of trusting a unit test. Invoke it when the user asks you to render, screenshot or verify the TUI.
---

# Testing the pmtui UI with tmux

pmtui is a full-screen ratatui app. It takes over the terminal, so you cannot run it directly and
read its output. Run it inside a tmux pane that you own. Drive it with `send-keys`. Read the
rendered screen back with `capture-pane`.

pmtui is file-based. It renders from a registry and from each project's `.project-state/`, and it
needs no running `pmd`. So a scratch registry that you seed first gives you any row state you want,
with nothing else running.

## The quick route: the laptop UI gallery

For routine visual review, use the gallery harness in the repository. Try it before you build a
fixture of your own:

```bash
scripts/pmtui-dogfood.sh
scripts/pmtui-dogfood.sh --size 100x28
```

It drives the real integration-test `pmtui` over private tmux sockets. It writes plain captures and
ANSI captures under `target/pmtui-ui/<UTC timestamp>/`.

The default matrix holds three sizes. `80x24` is a constrained terminal. `100x28` is the primary
13-inch laptop target. `120x32` is a maximized laptop terminal.

The gallery captures the main dashboard, dense Task columns, and the Task inspector, create and
Answer states. It also captures the Needs You answer surface, a long Message composer, the switcher
and the help overlay. Use `--sizes 63x24,64x24,89x28,90x28,97x28,98x28` to review the responsive
boundaries.

The gallery stages `pmtui` without a sibling `pmd`. It uses typed scratch state. It launches only
inert shell panes. It tears down both unique tmux servers through the integration fixture. It
cannot reach the live registry or the live socket.

Work through the steps below in order. Everything lives on scratch paths, so a test run cannot
touch the user's real projects or their real `pmd` tmux server.

## Safety: read this before your first `tmux` call

- **Never pass `--socket pmd`.** `pmd` is the user's real private tmux server. A test that attaches
  to it can kill or disturb a live agent pane. Always use a scratch socket name, such as
  `pmtuitest-inner` below.
- **Never point `--registry` at `~/.config/pmd/registry.json`.** That file is the user's real
  project list. Always use the scratch `registry.json` that you write under `$SCRATCH`.
- **Two tmux servers, and both are scratch.** `tmux -L pmtui-test` hosts the pmtui process, so that
  you can drive it. `--socket pmtuitest-inner` is the private server that pmtui would launch agent
  panes on. Kill both servers in cleanup.
- **A seeded idle row launches nothing.** A render test with `mode: "agent_loop"` rows and no `pmd`
  shows the rows and starts no agent. The `n` key creates a session, and `enter` attaches. Both DO
  launch a real `claude` or `codex` REPL. Press them only to test that path, and clean up the REPLs
  afterwards.

## 1. Build the REAL binary

```bash
cargo build          # produces target/debug/pmtui and target/debug/pmd
```

Do this after every code change that you want to see. `cargo test --bins` does NOT update
`target/debug/pmtui`. It builds a separate test-harness binary, and it leaves the real one STALE.
So you would render your OLD code and not know it. When a fix does not appear, suspect a stale
binary first. Run `cargo build`, then render again.

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

Without a `.project-state/config.json` the tier is unknown. The row then shows `[?]` and
`autopilot ?`. Without a running terminal the row is `offline`, and it groups under
`PAUSED / OFFLINE (n) · not running`. That is enough to test the head line, the help overlay, the
row grouping, the glyphs and the chrome. Seed `config.json` as in step 6 to render a concrete
`standard` tier or `autopilot` tier. Seed it also to exercise a driven state or an attention state.

## 3. Launch pmtui on a scratch tmux server

```bash
tmux -L pmtui-test kill-server 2>/dev/null; sleep 0.3
tmux -L pmtui-test new-session -d -s ui -x 140 -y 42 \
  "cd '$SCRATCH' && ECC_GATEGUARD=off '$BIN' --registry '$SCRATCH/registry.json' --socket pmtuitest-inner"
sleep 2      # let the first frame render
```

- `-x` and `-y` fix the pane size, so the layout is reproducible. Test more than one size. `140x42`
  is wide and shows everything. A narrow size such as `100x24` exercises the responsive shedding.
  There the age and the `chat` chip drop out, because the pane is too narrow.
- `ECC_GATEGUARD=off` stops the GateGuard hook. The hook would otherwise stall a child pmtui.

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

Send one key. Sleep for about 0.5 seconds. Then capture. The `?` overlay is the source of truth for
the keys:

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

Send a literal key name without quotes, such as `Enter`, `Escape`, `Up` or `PgDn`. Quote a bare
glyph such as `'?'`, so that the shell does not expand it.

## 6. Set up a specific render state (optional)

Idle standard rows cover a lot. For a driven state, an attention state or a report state, seed
files under `$SCRATCH/.project-state/` BEFORE you launch. One writer owns each file, as the README
table states.

- **An autopilot or driven row.** Write `config.json` with the autopilot tier. Then
  `daemon::pmd_drives_row` reports the row as driven, and the row shows `[A]`. The row groups under
  `AUTOPILOT` only while its terminal is live. A row with no terminal stays under
  `PAUSED / OFFLINE`.
- **An open decision.** Write `stops.json` with a stop that carries a `risk_class`. You then get
  the needs-you badge, the filled row, and the contextual `s` Answer overlay with content.
- **The agent's `report` section and status chip.** Seed the agent-loop ledger or marker that the
  preview reads, which is `AgentLoopState` and `last_status`.

Match each on-disk shape to the structs in `src/state/`, `src/job.rs` and `src/pmstate/`. Grep for
a unit test that builds the same record, and copy its known-good literal.

## 7. Clean up, always

```bash
tmux -L pmtuitest-inner kill-server 2>/dev/null   # any agent panes pmtui launched
tmux -L pmtui-test    kill-server 2>/dev/null      # the pmtui host server
rm -rf "$SCRATCH"
# If you pressed n/enter and spawned REPLs, confirm none linger:
pgrep -af 'claude|codex' | grep "$SCRATCH" || true
```

Kill the scratch servers even when pmtui exited already. A detached `new-session` server outlives
the app. A `pmtui-test` server left running wastes a socket. A `pmtuitest-inner` server left
running can leave an orphaned `claude` or `codex` REPL.

## Gotchas

- **A stale binary.** This is the most common wasted render. When the output contradicts your edit,
  run `cargo build` and render again, before you debug anything else. See step 1.
- **A capture taken too early.** A capture before the first frame returns a blank or half-drawn
  screen. Sleep 2 seconds after launch, and about 0.5 seconds after each key.
- **Trailing spaces.** ratatui pads every line to the pane width. Pipe the capture through
  `sed -e 's/[[:space:]]*$//'` when you want to compare text.
- **Width.** A bug can appear at one size only. Run steps 3 to 5 again at a narrow width, to catch
  truncation and responsive shedding.
