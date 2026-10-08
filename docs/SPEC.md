# agent-manager - Product Specification

**Status:** living specification for the product on `main`.

**Purpose:** This document tells you what the product does and what it promises. It does not tell you how
the product is built.

| For this information | Read this document |
|---|---|
| How to install and start | [`README.md`](../README.md) |
| How to use the product each day | [`GUIDE.md`](GUIDE.md) |
| Commands, file formats, provider arguments, schemas, code structure, test limits | [`AGENTS.md`](../AGENTS.md) |

> **Language of this document.** This document uses ASD-STE100, Simplified Technical English.
>
> It keeps three words from RFC 2119: MUST, MUST NOT and MAY. STE approves all three. It does not use
> SHOULD, because SHOULD does not give one clear strength. Each requirement here is a MUST or a MAY.
>
> Section 3 lists the Technical Names and the Technical Verbs. "Implementation-defined" means that the
> promise stays true, but the mechanism can change.

---

## Table of Contents

1. [Product intent](#1-product-intent)
2. [Scope](#2-scope)
3. [Product model](#3-product-model)
4. [Features](#4-features)
5. [Modes and operating loop](#5-modes-and-operating-loop)
6. [Reports and continuity](#6-reports-and-continuity)
7. [Who decides, and safety](#7-who-decides-and-safety)
8. [What the person can see and control](#8-what-the-person-can-see-and-control)
9. [Lifecycle, saved state and failures](#9-lifecycle-saved-state-and-failures)
10. [Future directions](#10-future-directions)

---

## 1. Product Intent

An agent that works alone has two opposite faults:

- it stops the person to ask about routine choices that the goal already answers; or
- it becomes idle, blocked, repetitive or unavailable, and does not make this clear.

agent-manager keeps Claude and Codex sessions at work on a goal that a person writes. It sends only the
decisions that a person must make to that person.

agent-manager adds automation above the permission modes of the engines. It does not replace those modes,
and it does not give permission to use a tool.

The product is correct when all of these are true:

- routine work that agrees with the goal continues without constant attention;
- the limits on what the product can decide stay in force;
- the person can attach to the same conversation at any time;
- the person can see the progress, the open decisions, the automatic answers and the failures; and
- a restart does not repeat work, lose an answer, or change who decides.

## 2. Scope

agent-manager is a local supervisor for coding agents. You install and authenticate those agents
yourself.

### In Scope

- persistent Claude and Codex sessions;
- the Standard mode and the Autopilot mode;
- nudges, monitoring, and detection of no progress;
- decisions that a worker raises, and the terminal menus that the product can read;
- a read-only decider that uses the goal to answer routine decisions;
- child jobs that an agent can give independent work to;
- a permanent decision history, and escalation to a person;
- more than one session, including sessions in the same project directory; and
- controls to attach, pause, resume, restart, remove and configure.

### Not in Scope

- a remote control plane, or one for more than one user;
- a general terminal or a general process manager;
- a workflow engine that makes its own plan;
- publishing, merging, deployment, use of credentials, payments or destructive actions without a person;
- a statement that a project is complete, without a person;
- any way around the permission, trust or command approval of Claude or Codex; or
- a dashboard that hides the agent from the person.

## 3. Product Model

### Roles

| Role | What it does |
|---|---|
| **Dashboard** | the view and the controls for the person |
| **Supervisor** | the driver for Autopilot sessions. It is optional. |
| **Worker** | the Claude or Codex agent that does the project work |
| **Decider** | a read-only consult that answers one routine decision from the goal and the project |

### Technical Names

| Name | Meaning |
|---|---|
| **managed session** | one terminal, one conversation, one goal, and the state for them |
| **persistent terminal** | the terminal of a managed session. It stays after the agent ends a turn. |
| **job** | a headless run that does one task, reports a result, and exits |
| **fork** | a new managed session that starts from the conversation of another one |
| **stop** | a decision that the worker cannot make, and that it sends out |
| **goal** | the text that tells the worker what to achieve. A person writes it. |
| **directive** | standing text that makes the decider more strict. A person writes it. |
| **cadence** | the time between two nudges |
| **report** | the current state that the worker writes after each wake |
| **continuity** | bounded notes that help the worker start again |
| **request** | the file that asks for a child job |
| **receipt** | the file that answers a request |
| **registry** | the list of managed sessions |
| **composer** | the field in which the person writes a message |
| **preview** | the panel that shows the terminal of the selected session |
| **worktree** | a second checkout of one git repository, on its own branch |

### Technical Verbs

| Verb | Meaning |
|---|---|
| **to attach** | to connect a person to a persistent terminal |
| **to nudge** | to send the operating message to a worker |
| **to spawn** | to ask for a child job |
| **to escalate** | to send a decision to a person |
| **to retire** | to stop a job, remove its row, and delete its own state |
| **to stage** | to hold a session that a request still creates |

### The Managed Session

The unit of the lifecycle is the managed session. It is not the project directory. Each managed session
owns:

- one persistent terminal and one conversation;
- one goal;
- one mode, Standard or Autopilot;
- the engine and the model for the worker and for the decider;
- the cadence and the monitoring state;
- the current status, the next step, the open stops and the continuity;
- the answers that wait for the worker; and
- a bounded history of decisions and events.

More than one managed session MAY use one project directory. The product MUST keep their identities,
terminals, control state, reports, decisions and histories separate.

A session MAY have a parent. A fork names the session that it came from. A child job names the session
whose agent asked for it. A child job MUST NOT be a parent. At most five rows MAY name one parent.

### Session Identity for the Worker

Each launch of a persistent terminal tells the worker which session it is in. The terminal environment
carries the session id, the directory of that session's state, and the path of the dashboard program. The
product omits the last value if there is no dashboard program.

These values send a worker's requests to its own session. They are not authentication. A terminal that
started before the session had these values gets them at the next launch.

### Child Jobs

A job is not a session that a person talks to. It is one headless run. It does its task, it reports a
result, and it exits.

A job runs in its own terminal. A person can attach to it and watch it, and the job continues if the
dashboard restarts. A job gets no spawn procedure and no session environment, so it cannot spawn.

A job ends when its process stops. This is a fact.

- The product MUST get the result from the event stream of the run.
- The product MUST NOT get the result from a file on disk.
- The product MUST NOT use idle time to decide that a job is complete.
- The product MUST NOT need a final step from the agent to find the result.
- The product MUST refuse a result that is not valid. It MUST NOT guess the result.
- If a run leaves no result that the product can use, the outcome is `ended_without_result`.

The preview of a job shows the event stream of the run. The dashboard draws the text of the agent, the
tools that it ran, and how the run ended. If the person attaches, the terminal shows the raw stream.

A job runs with the same permission mode as the agent that the supervisor drives in that directory. In
that mode the engine approves safe actions and stops only on dangerous ones. A job gets no more trust than
the agent of its parent.

The dashboard MUST refuse resume, restart, Autopilot, message and fork on a job row. A job has no
conversation, and nobody reads a message to it.

### Job Isolation and Integration

The product gives each job its own worktree if the project is a git repository. The worktree starts on its
own branch from the current HEAD of the project. Children run at the same time in one project, so one
shared checkout lets them write over each other. A branch belongs to one request, because the product uses
a session id again after a job retires.

If the project is not a git repository, or if the product cannot make a worktree, the job runs in the
project directory. The product then says so.

The product tells the job to put its work in one commit. The directory does not continue after the row,
but the commit does.

Isolation is not integration. The product MUST NOT merge the work of a child by itself. The receipt names
the branch, the commit, the files that changed, and any work that the child did not commit. A person then
applies that commit, and:

- the dashboard MUST ask the person to confirm first;
- the dashboard MUST refuse while the checkout of the person has changes to tracked files; and
- if there is a conflict, the dashboard MUST stop the operation and leave the checkout as it was.

The product MUST NOT delete work that nobody committed. It removes a worktree only if that worktree holds
nothing except commits. If the child left changes that it did not commit, the product keeps the worktree,
the row and the state. This rule holds for every outcome, and for each actor who ended the job. The branch
always continues, because a commit can have no other home.

### Job Outcomes

| Outcome | Meaning | What happens to the row |
|---|---|---|
| `done` | the run reported success | retired, except if it left work that nobody committed |
| `needs_human` | the run stopped on a decision for a person | kept, and does nothing, until a person clears it |
| `failed` | the run reported that the task failed | kept, and does nothing |
| `ended_without_result` | the process is gone and left no usable result | kept, and does nothing |
| `cancelled` | a person or an agent stopped it | retired, except if it left work that nobody committed |

To retire a job is to stop its terminal, remove its row, and delete the state of that session. It deletes
nothing else. The receipt of the parent holds what the run achieved.

- The product MUST NOT retire the session of a person this way.
- The product MUST NOT retire a job while a person is attached.
- The product MUST NOT retire a job before readiness records its own outcome.
- If a person removes a job row, this is a cancellation, and the product answers it as one.

The product MUST answer a job one time only. It reads the answer from state that retirement deletes, so
the recorded answer wins over a later answer. One harvest runs at a time.

A job whose readiness never finished is still complete. Readiness needs two live observations. A job that
did its task, or that died on a bad argument, can finish before those two observations. What the run left
is the answer. If a job asks for a person, the product leaves it as it is.

## 4. Features

### Child Jobs for a Worker

A worker asks for a child job with one command. That command publishes one request into the state of its
own session. It then waits a bounded time for the result of the child. It continues to wait after the child
starts, because the start of a child is not a result.

A second command lists each child that this session asked for, with the state and the result of each one. A
parent cannot know how long a child needs, so each wait is a guess. The pattern is: ask for the children,
do your own work, then ask about all of them together.

A third command stops one child.

These guarantees hold for all three commands:

- Each command MUST read only the environment of its own session. It MUST NOT touch the registry, the
  terminal multiplexer, the dashboard lock, or the home directory of the person. Each command therefore
  works in a sandbox with a read-only home directory.
- Each command is read-only except for the one request that it publishes.
- The product MUST publish a request one time only, and MUST NOT write over it. The same request id with
  the same arguments asks about the same request. The same request id with other arguments is a conflict,
  and the published request stays.
- A receipt continues after its request. A repeat after cleanup therefore returns the final answer, and
  MUST NOT make a second child.
- If a command gets no request id, it makes one and reports it.
- A command MUST NOT show the Message. A receipt MUST NOT hold the Message.
- To ask twice for a stop is the same as to ask one time. The command reports that it asked for the stop,
  and the dashboard does the stop.
- The product MUST refuse a request id that this session never asked for.
- If a job already ended, the product reports that outcome, and not a cancellation.

A request command refuses before it publishes if:

- it is not in a managed session;
- the title or the name is not valid;
- the request is too large for the dashboard; or
- it cannot write the request.

Each outcome is one line of text, or one JSON document. The exit status separates a final result, work
that continues, a failure, and a bad command line. A command that prints JSON prints it on a nonzero exit too. An agent MUST read that document. An agent
MUST NOT use the code alone.

`AGENTS.md` gives the names of the commands, their arguments, their output and their exit codes.

### Concurrency

Five children run at the same time. A sixth request waits.

This limit is on concurrency. It is not a limit on how many children a parent can ask for. A request that
waits reports that it is in progress. It starts at the first opportunity after a sibling ends. The product
MUST NOT write anything that an agent must then act on.

### To Stop a Child

The dashboard asks the run to stop. It then waits a short time. It stops the terminal only after that
time. The engines give no way to ask for a final result, so time is all that a stop can give.

- The dashboard MUST NOT wait for a process to die.
- If a job reported before the stop arrived, the product keeps the outcome of the job.

### The Spawn Procedure

Each launch that names the dashboard program first installs the spawn procedure into the project directory
of the session. It installs the procedure in the form that the engine of the worker can find. A launch
that names no dashboard program installs nothing, because the command in the procedure could not run.

The procedure tells the worker to:

- give away only work that is independent;
- write one Message that names the target, the change, the limits, the owner and the acceptance test;
- ask for several children before it waits for any of them;
- read the JSON result even if the exit status is not zero;
- ask about a request that waits under the same request id, and never under a new one; and
- stop and tell the person if an outcome is not known. It MUST NOT spawn a replacement.

To install the procedure MUST NOT stop a launch, and MUST NOT change the agent instructions of the person.

### The Dashboard

The dashboard lets the person:

- make a session and configure it;
- give one first message to a new Standard session;
- look at the current work, the waiting state, the terminal, the decisions and the history;
- find and select a session with a quick switcher;
- attach to the terminal of the worker, or send a message into it;
- answer a decision;
- apply the commit of a job to the checkout of the project;
- change between Standard and Autopilot;
- change the goal, the directive, the cadence, the worker model and the decider; and
- pause, resume, restart or remove a session with separate actions.

The dashboard stays useful if the supervisor is not available. It MUST say that the supervisor is not
available. It MUST NOT show the automation as active.

### Views

The dashboard has three numbered views of the same state:

| View | Contents |
|---|---|
| **Session view** | the workbench, with the terminal first |
| **Task view** | each item in one group only: Paused, Needs You, Pending, Autopilot or Working |
| **Settings view** | one row for each setting |

A number key selects a view. It does not switch between two views.

The groups in the Task view are observations of the live state. They are not a workflow status that a
person sets. A card MUST NOT move into a state that the product does not have. There is no Done group. A
request to confirm that the goal is complete stays in Needs You until the person closes the session.

The Session view shows what needs attention first. Decisions for a person, and automation that is not
available, come before routine work and idle counts. A session is offline if its terminal is not available
and it does not wait for a person.

If the terminal is narrow, navigation stays and actions go. The dashboard MUST keep the bindings in the
Help overlay. If the terminal is too narrow for two columns, the dashboard uses one column. If the
composer is then open, the composer stays visible.

The selection, the position in the terminal, and the drafts in the composer all continue after a resize.

The dashboard shows emphasis as coloured bold text. The dashboard MUST NOT use an animation. It draws
again only because the state changed.

`AGENTS.md` gives the layout rules and the order in which elements yield.

### To Make and Name Sessions

After the dashboard makes a session, it selects that session and shows its terminal at the newest
position. A change of selection also starts the preview at the newest position. The preview MAY make a
large empty area smaller, but it MUST keep the output and the frame of the worker.

The Standard create form MAY take one first message. The product submits it as the first turn of the new
terminal.

- It MUST use the same terminal and conversation that attach uses later.
- It MUST NOT make a second worker, a task queue, or a second lifecycle.
- Resume and restart MUST NOT send it again. A record of the launch alone MUST NEVER allow this.
- If the terminal cannot accept the message, the form stays open and says how much text to remove. The
  product MUST NOT make a session that cannot start.

The Autopilot create form has no message field. Its goal and its first nudge give the first instruction.

Both forms take an optional Name. The Name is display text only. The generated id stays the identity of
the session. To rename changes only the label. The id keeps the terminal names, the paths of the state,
the locks, and the route of each action. An empty name restores the id.

The dashboard fits each label to the cell width of the terminal. A wide character MUST NOT push the state
or the mode of a row off the screen.

The quick switcher matches the identity of a session and its project directory. It MUST NOT change the
registry. It MUST NOT make a task, a ticket, or a second session.

### To Fork a Session

A fork makes a second managed session from a known conversation, in the same project directory. The source
session does not change.

The child copies the goal, the directive and the model configuration. It starts in Standard mode, and the
person can select Autopilot later. A fork MUST NOT make a git branch, a worktree, a task or a ticket.

- A live source MUST be free of a person and stably idle before a fork.
- If the product cannot prove the identity of the conversation, it refuses the fork and says what to do.
- A fork either publishes a child that runs, or removes what it staged.
- If the product cannot stop the terminal of the child, it keeps the disabled row. It MUST NOT hide a live
  process.
- A fork takes the parent of its source, so a fork of a child job cannot get past the depth limit. It
  counts against the limit of that parent.

### Messages and Prose Fields

The composer is a band inside the dashboard, not a separate window. It uses the normal input path, which
includes the last check that the terminal is live. It MUST NOT write state that the worker owns.

If a stop is open on a session that the supervisor drives, the same key opens the Answer surface.

On a session that nothing drives, an open stop changes nothing. The product does not offer the stop, and
the key stays the composer. The question of the agent is already on the screen of its own terminal, and
the person answers it there.

**The product MUST NOT ask a person to change the autonomy of a session before a key works.**

Three fields are full editors: the message, the goal and the directive. Each one holds prose that an agent
or the decider acts on. Each one is therefore a buffer of more than one line. A field of one line is for
one value, such as an id, a time or a query.

The goal field and the directive field open with the current text in them, so the person edits in place. A
save with no text keeps the current text. To remove a directive stays a separate and clear action.

The dashboard keeps a message that the person did not send, for each session. It restores that message and
the cursor when the session opens the composer again. A send that works clears the message. A refusal or a
failure keeps it. The product MUST NOT send a draft again after the dashboard exits.

An open composer stays with its session. While the composer is open, a key MUST NOT move the selection, and
a pointer action MUST NOT move it.

`AGENTS.md` gives the keys of the editors.

### Status Log

The menu at the bottom shows the newest status line, and nothing older.

The dashboard also adds each status to a log file with a time stamp. That file sits beside the registry
that the dashboard owns, so a scratch registry gets a scratch log. Rotation keeps the size of the log
bounded.

A write to the log is best effort. The dashboard drops a write that fails, because it must draw the frame
in any case. The Help overlay names the file, so the person can find it.

### Theme and Preferences

Each colour in the dashboard comes from a theme. No colour comes from the ANSI slots of the terminal. Every
cell of the frame is the colour of the theme. A "default colour" in the output of an agent reads as the
default of the theme. A colour that the agent selected passes through.

The product MUST show severity with a symbol, a word, a badge and a colour together. One colour with low contrast MUST NOT carry the
meaning alone. The author of the theme owns the contrast.

The Settings view is a table of settings. Each row has the name of the setting, the value in force, and one
phrase that says what the setting controls. The product offers only the values that it can apply.

- A list of values opens on the value in force. To move in the list changes nothing. The value changes
  only on commit.
- While a list is open, the list owns the keyboard.
- The Settings view routes no lifecycle key. A key there MUST NOT reach the selected session.

A setting applies first and saves second. The product reports each failure apart from the other, because
"It does not work" and "It will not continue after a restart" are different facts.

The preferences belong to the person. They sit beside the registry, so a scratch registry MUST NOT write
over the preferences of the person. The product gives each field a default, so a file from another version
still loads. The product reports a file that it cannot read, and MUST NOT replace it without a message.
The product applies the saved theme before the first frame.

`AGENTS.md` gives the colour roles and where RGB values enter the frame.

## 5. Modes and Operating Loop

### Modes

| Mode | Who drives the terminal | Routine choices | Choices for a person |
|---|---|---|---|
| **Standard** (default) | the person | in the attached session | the person |
| **Autopilot** | the supervisor, on a cadence | MAY continue after a valid decider answer | the person |

A change of mode MUST keep the terminal and the conversation. To turn Autopilot off stops the input from
the supervisor. It does not pause the worker, and it does not restart the worker. Pause, restart and
remove are separate actions.

### The Operating Loop

For each Autopilot session that is enabled, the supervisor:

1. applies the control changes that the person made;
2. makes sure that the persistent terminal exists, if that is safe;
3. reads the newest report of the worker;
4. routes the decisions of the worker and the terminal menus that it can read;
5. observes whether the worker is at work, waiting, monitoring, blocked or unavailable; and
6. nudges an idle worker if the cadence is due and the terminal is safe to drive.

The supervisor MUST NOT make a second conversation for Autopilot. The person and the supervisor use one
persistent terminal.

If an Autopilot session waits for a decision from a person, and the product cannot read its mode setting,
the supervisor stays available. It MUST NOT type until it can read the setting again. An explicit Standard
setting stays in force.

### Who Owns the Input

A person who attaches to a session, or who types into it, owns the input of the terminal. In that period
the supervisor MUST NOT type, MUST NOT submit a choice, and MUST NOT send an automatic answer. The work
and the answers that wait stay saved until the automation can continue safely.

The product MUST serialize all automatic input to a terminal. The last checks for a person and for a live
terminal MUST cover the complete submission.

An automatic send of prose is complete only after the terminal accepts it. Some engines leave the pasted
text on the screen. For those engines the supervisor MAY try again a bounded number of times. It MUST look
at the screen between two tries. It MUST stop as soon as the draft clears, or work starts, or another
surface opens. A try MUST NOT confirm a surface that opened later.

### At Work, Waiting and Monitoring

A live process is not proof of work. The product separates these states:

| State | Meaning |
|---|---|
| **working** | the worker makes a turn now |
| **waiting** | the worker is live and idle at its prompt |
| **monitoring** | the worker gave up its turn, and other work or a check is still open |
| **needs you** | a decision for a person is open |
| **unavailable** | the worker or the supervisor cannot continue |

The product treats terminal activity that is not clear as work. It MUST NOT treat the last frame of a dead
terminal as an idle prompt.

An accepted monitoring report means that the worker gave up that wake. The dashboard MUST show the next
check after the terminal is stably idle, even if an optional turn-end signal did not arrive. The
supervisor MUST then clear the baseline of that wake, so that a missing signal cannot make an idle session
look stalled. A live busy terminal is newer evidence, and the product continues to show work.

**On a session that the supervisor does not drive, live activity wins over recorded state.** Nothing
advances the record of that session, so the record stays as it was when the mode last changed. A working
terminal therefore sets the state that the dashboard shows. The open decision stays in the record and
stays visible. Only the claim that the product waits for a person goes away.

A session that is idle at its prompt with an open decision still needs a person. A decision on a session
that the supervisor does drive keeps its place.

A turn of a worker is finite. Long work MAY enter the monitoring state only if that work:

- another process can observe;
- has a bound; and
- is safe if a person pauses, restarts or removes the session.

Work that needs interaction, approval, a destructive action, parallel edits, or later cleanup by its owner
stays in the foreground.

### Bounds and Detection of No Progress

The cadence and the monitoring delays have bounds, and a person can configure them. A cadence that a
person sets wins over a later proposal from the worker.

These signals MAY raise a decision for a person:

- activity that continues with no progress;
- reports that do not arrive, again and again;
- the same plan, again and again;
- a session budget that is now empty; and
- wakes that do not change the project, again and again.

If the evidence for one signal is missing, the product turns off that signal. It MUST NOT invent a
failure.

The inactivity window of a busy worker restarts only if the content of the terminal changes in a way that
has meaning. A spinner, a counter, a token count, a placeholder and a clock are not progress.

Progress on the terminal never pays a report debt, never gives permission, and never allows input. It only
stops the product from reading active work as a silent stall.

#### Proof That a Turn Ended

**Each engine gives different proof that a turn ended. The product MUST use evidence. It MUST NOT guess.**

- Some workers show a fixed activity symbol for the whole turn. For those workers the product reads the
  terminal: when the symbol goes and the terminal is stably idle, the turn is over. The product MAY then
  ask a worker that owes a report for one.
- Some workers keep an idle prompt on the screen inside a turn. For those workers only an explicit
  turn-end signal counts.

The product MUST NOT take either reading from the documents of a vendor. It MUST measure the reading
against a long tool call that prints nothing. That case makes an idle terminal most misleading.

A turn-end signal is optional, and it MAY be unreliable. A behaviour that a session needs MUST NOT depend
on it.

#### The Ceiling on an Owed Report

While a worker owes a report, the supervisor MUST NOT type into it. An idle terminal can be a long tool call
that prints nothing. That hold MUST NOT continue without end.

A worker that does not report for a bounded time after the last nudge raises a decision. A person can
dismiss that decision. It says only that the worker did not report.

The product MUST measure the ceiling on the owed report itself. Output on the terminal MUST NOT defer it,
and a restart of the supervisor MUST NOT defer it.

The ceiling sits above the inactivity limit, because the finer signals act sooner. Those signals are a turn
that ended with no report, and a terminal that is not active.

An automatic mechanism MUST NOT end this hold from the content of the terminal.

##### What the Decision Says

The decision MUST name the cause, if the product can prove the cause. A turn-end signal that never fired
is such a cause. A turn-end signal that is far behind the reports of the worker is also such a cause. In
these two conditions the decision MUST name the signal. It MUST give the counts that show the fault, and
it MUST tell the person to restart the session.

These two conditions are limits of the product, and not faults of the worker. A session keeps such a
limit for as long as it runs. Each check-in then waits for the full ceiling. The decision MUST show this
as a limit of the product.

The product MUST NOT name this cause in three conditions:

- The worker does not need the signal, because its terminal shows when a turn runs.
- The session is new, and a signal that is absent is normal.
- The signal keeps pace with the reports of the worker.

In these three conditions the decision says only that the worker did not report.

#### Diagnostics for the Turn-end Signal

The diagnostics MUST report a turn-end signal that is absent, or that is behind the reports of the worker.
A worker cannot report more often than it ends turns. A session that depends on a dead signal loses each
recovery that it needs.

The diagnostics and the ceiling MUST judge the signal by one rule. Two rules can disagree. The diagnostics
then describe a different fault from the fault that the supervisor acted on.

### Limits on a Session

The limits on a session escalate. They MUST NOT stop the persistent terminal, and they MUST NOT say that
the goal is complete.

## 6. Reports and Continuity

### The Report of the Worker

The worker publishes one current report after each wake. The report says:

- whether the worker is at work, monitoring or blocked;
- what changed, or what the worker learned;
- the next step;
- any change to the cadence that the worker asks for; and
- any decision that stops more progress.

A report is a proposal. It is not authority. The supervisor decides how a report changes the schedule, the
automation and the attention of the person.

Each field of a report has one speaker:

- The worker writes the status, the question and the next step. They are about its own work.
- An option in a decision is a possible answer from the person. The dashboard shows it for the person to
  select, and the supervisor returns it without a change.

The worker MUST write each option from the point of view of the person. The worker MUST write each action
as a complete instruction, so that the selected answer is clear outside the list of options.

Only a report with new meaning MAY change the state. A report that repeats an earlier one does nothing.

The product MAY read a recent incomplete write again. Data that stays invalid MUST NOT answer a decision,
and MUST NOT become progress.

The supervisor owns the question of which report is newest. A sequence number from the worker is diagnostic
only. It MUST NOT change the schedule, the match between turns, or a later report.

**A worker that believes that the goal is complete MUST ask a person to confirm this.** A worker MUST NOT
say that a managed session is complete. A supervisor and a decider MUST NOT say this.

### Continuity

The worker MAY keep bounded continuity notes, to start again correctly after a compaction or a restart.

The notes MAY give these items:

- the work that is complete, and the current work;
- the blockers;
- the long work that is open;
- important references; and
- the next actions.

Continuity is context that the product does not trust:

- it stays below the goal and the current report;
- it MUST NOT ask for a decision, and MUST NOT answer one;
- it MUST NOT change the policy, the cadence, the progress or the completion;
- it MUST NOT hold a secret; and
- the product MUST NOT copy it into an automatic instruction.

### Contents of a Nudge

An Autopilot nudge MAY hold only:

- the goal that the person wrote;
- the saved status and next step of the worker;
- the answers from a person that wait, and the valid results of the decider;
- fixed reminders about the operating procedure; and
- fixed signals that recover a missing report or a repeated plan.

The nudge MUST name the installed worker procedure in the form that the engine reads, so that the engine
loads it at each wake. If that procedure is not available, the nudge MUST NOT give a reference that does
not work. It MUST carry a short set of reporting rules instead.

A nudge MUST NOT hold any of these:

- the contents of the continuity notes;
- the history of the audit;
- free narration from the supervisor;
- state from the dashboard that is not related; or
- the directive.

The directive limits the decider. It does not instruct the worker.

The same allowed inputs MUST give the same nudge, whatever else is in the history of the supervisor.

## 7. Who Decides, and Safety

Autopilot handles a decision in three separate layers:

1. **The fixed boundary.** Categories and explicit effects name the decisions that stay with a person.
2. **The goal-aware answer.** The read-only decider takes the routine decisions that are eligible.
3. **The permission of the engine.** Claude or Codex still enforces its own permission, trust and command
   approval for each tool action.

To pass one layer MUST NOT give a way past another layer.

### Evidence from the Worker, and Fixed Policy

For each stop the worker reports a kind, a risk label, and evidence about the scope, the reversibility and
the owner. These labels are evidence. They are not authority.

- A worker MUST NOT make a decision for a person automatic with a label of low risk.
- A hard label on a routine question stays visible in the audit. By itself it MUST NOT stop a person.
- Evidence that is unknown or partial MAY go to the decider. The product MUST NOT read it as permission,
  and MUST NOT escalate on it alone.

These decisions stay with a person:

- to publish, to merge, to land, to release or to deploy;
- credentials, access, trust, permissions, payments or changes to an account;
- destructive operations, and operations that nobody can reverse;
- operations that are external or privileged;
- changes to security;
- confirmation that the goal is complete; and
- any decision that is still unsafe, or that the product cannot classify or validate.

### The Goal-aware Decider

The decider answers whether one routine decision moves the goal forward, and says what the worker must do.
The decider:

- is read-only, and has a bound;
- MAY look at the project to check the claim of the worker;
- MAY select only from the options that the product gives, or give one bounded answer;
- MUST NOT make the fixed boundary wider;
- treats unknown evidence as something to examine, and not as permission;
- gets the last accepted result and next step of the worker as progress context that it does not trust,
  apart from the status that the supervisor wrote and the earlier outcomes;
- MUST connect a positive answer to the goal and the evidence; and
- MAY refuse if the goal or the evidence does not make a routine choice safe.

The product validates each answer on its own. An answer that is malformed, invalid, interrupted or refused
sends only the current decision to a person.

The product reviews more than one routine decision in sequence. One answer authorizes one decision. A
failure or a refusal on one decision MUST NOT authorize a sibling, and MUST NOT escalate a valid sibling.

If the decider is not available, the decisions that wait stay visible and go to a person.

### The Directive

A person MAY set a standing directive for Autopilot decisions. A directive MAY only make the decider more
strict. It MUST NOT authorize an action, MUST NOT get past a boundary that belongs to a person, and MUST
NOT redefine the goal.

### Terminal Menus

The product MAY answer a routine menu in the terminal only if it can prove all of these:

- the menu is a bounded choice, and not a permission, trust or command approval surface;
- the options are clear;
- the live menu still matches the menu that the product examined;
- the control takes input; and
- the product can submit the option without free text.

A menu that changed, that is about permission, that takes no input, or that the product cannot validate,
stays with the person.

In the Answer surface the product MUST show the complete question and each option without a cut. If the
text is too long, it MUST scroll, and the answer field MUST stay visible.

## 8. What the Person Can See and Control

The dashboard MUST make the work and the automation easy to read.

### Keyboard and Pointer

The keyboard and the pointer are equal surfaces for each visible action:

- a click on a session row selects that row;
- a click on the title of the preview takes the same route as the keyboard, and MUST NOT make a second
  terminal;
- a row of the create form takes focus from a click, but focus MUST NOT change a value and MUST NOT move
  the cursor; and
- a click on a key chip runs the same command as the key.

Hit areas belong to one frame. The product MUST clear the old areas before it takes another pointer action. This applies after a resize, a
change of mode, an overlay or a full-screen view.

The pointer MUST keep each precondition, each confirmation, each input lock and each boundary. It MUST use
the same handlers as the keyboard.

### What the Primary View Shows

For each session the primary view shows enough to separate:

- active work from a live process;
- waiting from monitoring;
- an open decision for a person from an automatic decision in progress;
- a healthy supervisor from automation that is not available; and
- the current status from old history.

A decision debt that the product saved is named as pending. The product shows it as reviewing only while
its consult is live, with the time that passed and the number of decisions in the queue. Attention for a
person comes first.

The question, the policy evidence, the reason and the outcome live in the audit. The product MUST NOT
compress them into the primary status.

### The Audit

The audit of each session has a turn trace and a decision history.

The turn trace holds these items for each nudge:

- the report that answered the nudge;
- the time that passed;
- the state of the worker, and what the supervisor did;
- the status and the next step;
- whether a recovery changed the wake; and
- the grouped reasons why the product held back a wake.

The decision history holds these items:

- the question and the options;
- the evidence of the worker, and what the policy did;
- the configured decider, and the current consult;
- the decisions in the queue, in order;
- the valid answers, the refusals, the interruptions and the failures; and
- a bounded history of outcomes.

The audit records the inputs and the outcomes that a person can observe. It MUST NOT record the hidden
reasoning of a model, and it MUST NOT change a decision.

If the policy stops a consult, the audit says that the product did not call the decider, and names the
boundary.

### Diagnostics

The headless diagnostics give the health of these items:

- the installed supervisor and the registry;
- the project state;
- the worker procedure and the spawn procedure;
- the programs that the product needs;
- the notification channel; and
- the terminals.

The diagnostics also say whether the terminal multiplexer is new enough to give a managed launch its
session environment.

The diagnostics only observe. They MUST NOT do any of these:

- take ownership;
- repair a file;
- start or stop a terminal;
- send input;
- notify the desktop; or
- call a worker or the decider.

A warning means a capability that is degraded, or that did not start yet. A failure means required state
that is invalid or not available. The same result is available in a stable machine-readable form.

### Controls for the Person

The person can correct Autopilot with these controls:

- edit the goal;
- add or change a directive;
- answer the current decision;
- change the cadence;
- select Standard; or
- pause or restart the session.

A model list uses the catalogue of the provider if there is one. For an account with no catalogue, the
dashboard gives stable model names. It MUST NOT fall back to the default of the provider. The engine
enforces what the account can use.

Open stops and stuck states are saved dashboard state. A desktop notification is a best-effort signal. A
failure of that channel MUST NOT remove the state and MUST NOT resolve it.

## 9. Lifecycle, Saved State and Failures

### Who Owns What

The product writes each authoritative change of state as one atomic operation. Ownership follows the
concern:

| Owner | What it owns |
|---|---|
| **Dashboard** | the intent of the person, the lifecycle controls, the goals, the directives. It is the only process that turns a request into a session. |
| **Worker** | the reports and the continuity |
| **Supervisor** | the runtime state, the automatic decisions that wait, and the audit |
| **The process that launches a terminal** | the procedure files that it installs for that launch |

Each component builds its state again from disk after a restart. An observation in memory MAY need to
happen again. A restart MUST NOT do any of these:

- repeat an accepted report;
- send a used answer again;
- lose an answer from a person that waits; or
- change who decides.

### Isolation and Exclusive Control

- Each managed session has its own identity and its own lifecycle.
- A new session MUST NOT take the conversation, the progress, the decisions or the open work of a session
  that closed.
- To resume or to restart a conversation needs an exact identity.
- At most one supervisor drives one fleet. At most one dashboard changes it at a time.
- A second interactive dashboard MAY replace the current one only after the person confirms. It asks the
  current one to exit, and takes the released lock before it draws. To force an exit needs a separate
  confirmation. **A dashboard takeover MUST keep the supervisor and each project terminal.** A
  non-interactive second dashboard always refuses.
- The intent of a person to attach stops the automation from a race into the shared terminal.

### Lifecycle Actions

| Action | Guarantee |
|---|---|
| Stop or replace the supervisor | the worker terminals continue |
| Change the mode | the worker process and the conversation continue |
| Pause | the worker stops, and the automation turns off |
| Resume | the same managed session continues |
| Restart | the worker process restarts only after the product proves the identity of the conversation |
| Remove | the session closes, and its history MUST NOT become available for automatic reuse |

A session that a request still creates is staged. That request owns the only launch of the row. Each other
start route MUST refuse the row, so no second route starts it or sends its Message again.

The product MUST NOT treat saved state that is corrupt or unreadable as empty state that it can overwrite.

Before the supervisor drives a session, it makes sure that the worker has the current procedure. To refresh
the procedure MUST NOT restart the conversation, and MUST NOT discard it.

### Guarantees for a Spawn Request

A worker asks for another session only when it publishes a request in the state of its own session. The
running dashboard is the only process that turns a request into a row, and it answers in a receipt beside
the request.

Exactly one dashboard brokers one session list, so two dashboards MUST NOT both turn one request into a
child. A dashboard that does not hold that role advances nothing, and says so one time in its status log.

- The dashboard advances each request by at most one step for each frame, so a request MUST NOT block the
  dashboard.
- **The product MUST launch the Message of a request at most one time.** It records a claim before any side
  effect. A dashboard that takes over from one that died finishes each unfinished request from the row. It
  MUST NOT start the request again.
- The directory of a child stays inside the project of the parent, or inside a directory that a session in
  the list owns. The product resolves the path before it trusts it, so a symlink cannot lead outside. Any
  other directory is a refusal.
- The defaults come from the parent: its directory, its engine, its worker model if the engines match, and
  the first line of the Message as the title.
- The product removes a request whose receipt has been final for more than one week. It keeps a small
  record of the receipt, so a replay of that request id after the row goes MUST NOT make a second child.
- The product MUST NOT process a request from a session that is no longer in the list.
- The product answers a request that is malformed or too large as invalid. It MUST NOT guess.
- The dashboard writes the registry, the configuration of the child, and the receipts. It MUST NOT write a
  request, and MUST NOT write state that the worker owns. The supervisor MUST NOT read the spawn files.

### Failure Outcomes

The product handles a failure in the direction that keeps the control of the person and the work that
exists.

| Failure | What the product does |
|---|---|
| The activity of the worker is not clear | treat it as active; do not start a new turn |
| The terminal of the worker died | show the dead session; never type into its old frame |
| A person attaches during automation | give up the input; keep the work that waits |
| A report is old or repeated | ignore it; change no state |
| A report stays invalid | show a bounded failure; never make a stop weaker |
| The worker does not report or progress, again and again | raise a decision; do not stop the terminal |
| The evidence for a decision is partial or unknown | ask the decider; escalate only if the choice stays open |
| The decision belongs to a person, or an answer is invalid | send that decision to a person |
| The decider is not available | keep the decisions that wait, and show them |
| The identity of a session is missing or not clear | refuse the restart or the resume; do not stop the live terminal |
| The supervisor fails again and again | stop the tries; keep the terminal; show that the automation is not available |
| The saved configuration is corrupt | fail closed; do not overwrite state that the product can recover |
| A desktop notification fails | keep the dashboard state; continue without that channel |
| A worker procedure or a spawn procedure is old | refresh it before Autopilot drives; keep the conversation |
| The launch of a child is not clear | keep its row for a person; report an unknown outcome and how to attach; never send its Message again |
| The process of a job is gone with no usable result | report `ended_without_result`; name what is known; keep its row for a person |
| The product cannot make the worktree of a job | run the job in the project directory, and say so |
| The worktree of a job holds work that nobody committed | keep the worktree, the branch, the state and the row, for every outcome |
| A commit from a job conflicts | stop the operation; leave the checkout of the person as it was |
| The parent of a job is no longer in the list | retire the job; the work is over, and the log continues |

## 10. Future Directions

These are future changes to the product. They are not missing foundations.

| Direction | Contents |
|---|---|
| **Better evidence of a stall** | combine repeated blockers, total cost, and a better way to exclude changes that are not related |
| **Age of an escalation** | remind a person about a decision that waits a long time, or send it elsewhere, without a second stop |
| **Search of decisions across sessions** | review and filter the history of decisions across sessions, and keep the context of each session |
| **Paths that a child owns** | give a job a declared set of paths that it can edit, and let the engine enforce them |
| **One contract for an engine** | define one capability contract, so that a new engine needs one integration and not a change across the product |

---

*Keep this document the same as the behaviour that a person can observe. Put source maps, commands, file
formats, provider flags, schemas, test limits and runbooks in `AGENTS.md`, `README.md` or
`docs/GUIDE.md`.*
