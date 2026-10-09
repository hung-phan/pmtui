//! Launching the ONE persistent session and picking the conversation it runs on:
//! cold-start grace, mint-vs-resume, the registry seed, and the uuid itself.

use super::*;

// --- launch-once / cold-start --------------------------------------------

#[test]
fn fresh_idle_launches_persistent_session_once_without_nudging() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    // First drive: launches the pmloop session (cold-start), does NOT nudge.
    let out = fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        out,
        JobTick::Monitoring {
            until: START + LAUNCH_GRACE_S
        },
        "cold-start parks a short grace, not a nudge"
    );
    let launched = fx.driver.launched();
    assert_eq!(launched.len(), 1, "launched exactly once");
    assert_eq!(launched[0].0, sess);
    // Interactive claude argv: env prefix drops only CLAUDECODE,
    // `--session-id <minted uuid>`, NO `-p`/stream-json.
    let argv = &launched[0].1;
    assert!(
        !argv.iter().any(|a| a.starts_with("ECC_GATEGUARD=")),
        "{argv:?}"
    );
    assert!(argv.iter().any(|a| a == "claude"), "{argv:?}");
    assert!(!argv.iter().any(|a| a == "-p"), "{argv:?}");
    // First-ever launch with no id ⇒ mint ⇒ CREATE (--session-id), never --resume.
    let sid = launched_flag(&fx, &sess, "--session-id").expect("--session-id present");
    assert!(!sid.is_empty());
    assert!(
        launched_flag(&fx, &sess, "--resume").is_none(),
        "first launch creates"
    );
    // No nudge on the launch tick.
    assert!(
        fx.driver.sent_keys().is_empty(),
        "cold-start must not nudge"
    );
    // The ledger persisted the minted id + Monitoring{grace}; driver.json handle.
    let l = ledger(&fx);
    assert_eq!(l.conversation_id.as_deref(), Some(sid.as_str()));
    assert_eq!(
        l.run,
        JobRun::Monitoring {
            until: START + LAUNCH_GRACE_S
        }
    );
    let d: DriverState = state::read_json(&fx.paths.driver()).unwrap();
    assert_eq!(d.exit_reason, ExitReason::Running);
    assert_eq!(d.pane, sess);
    // A second tick while still alive (and before grace elapses) does NOT relaunch.
    assert_eq!(
        fx.sched.tick(&fx.driver, &fx.clock).unwrap(),
        JobTick::Monitoring {
            until: START + LAUNCH_GRACE_S
        }
    );
    assert_eq!(fx.driver.launched().len(), 1, "no relaunch while alive");
}

#[test]
fn the_persistent_terminal_carries_its_session_identity_and_pmds_pmtui() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let identity = tmux::ManagedEnv {
        session_id: SESSION_ID.to_string(),
        state_dir: fx.paths.state_dir(),
        pmtui_bin: None,
    };
    assert_eq!(
        fx.driver.launched_env(),
        vec![(sess.clone(), identity.clone())],
        "a scheduler pmd gave no pmtui omits PMTUI_BIN"
    );

    // pmd hands every runner the pmtui beside it; the next launch names it.
    fx.sched.set_pmtui_bin(Some(PathBuf::from("/opt/am/pmtui")));
    fx.driver.set_alive(&sess, false);
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        fx.driver.launched_env().last(),
        Some(&(
            sess,
            tmux::ManagedEnv {
                pmtui_bin: Some(PathBuf::from("/opt/am/pmtui")),
                ..identity
            }
        ))
    );
}

#[test]
fn a_typed_launch_failure_keeps_its_text_under_the_session_context() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    let base = ledger(&fx);
    fx.driver
        .arm_launch_error(&sess, tmux::LaunchError::NotOnPath("claude".into()));
    let error = fx
        .sched
        .ensure_session(&fx.driver, START, &base)
        .unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        format!(
            "launch persistent loop session {sess} for {SESSION_ID}: \"claude\" was not found on PATH — is it installed?"
        )
    );
    assert!(fx.driver.launched().is_empty());
    assert_eq!(ledger(&fx), base, "a failed launch leaves the ledger alone");
}

// --- conversation-id resolution -----------------------------------------

#[test]
fn claude_mints_and_pins_a_fresh_id_when_no_seed_or_ledger_cid() {
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    // Fresh mint ⇒ CREATE via --session-id (never --resume).
    let sid = launched_flag(&fx, &sess, "--session-id").expect("claude mints via --session-id");
    assert!(launched_flag(&fx, &sess, "--resume").is_none());
    assert_eq!(ledger(&fx).conversation_id.as_deref(), Some(sid.as_str()));
}

#[test]
fn relaunch_with_existing_ledger_cid_resumes_not_creates() {
    // The common path (session died / pmd restart): a relaunch must RESUME the
    // persisted id (`--resume`), NOT re-CREATE it (`--session-id` would error).
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // first launch → mint (--session-id)
    let minted = launched_flag(&fx, &sess, "--session-id").expect("first launch creates");
    // The session dies; a due tick re-ensures it.
    fx.driver.set_alive(&sess, false);
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.driver.launched().len(), 2, "relaunched");
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some(minted.as_str()),
        "relaunch RESUMES the persisted id"
    );
    assert!(
        launched_argv(&fx, &sess)
            .unwrap()
            .iter()
            .all(|a| a != "--session-id"),
        "a relaunch must not re-CREATE the conversation"
    );
}

#[test]
fn adopt_registry_seed_resumes_that_id() {
    // An adopted chattable-on-create seed is a conversation the human's first chat
    // already CREATED, so the loop RESUMES it (--resume), never re-creates it.
    let mut fx = setup(Tier::Standard, Engine::Claude, Some(300));
    fx.sched.set_registry_seed(Some("seed-U"));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("seed-U")
    );
    assert!(
        launched_flag(&fx, &sess, "--session-id").is_none(),
        "adopt resumes"
    );
    assert_eq!(ledger(&fx).conversation_id.as_deref(), Some("seed-U"));
}

#[test]
fn ledger_cid_wins_over_registry_seed_and_resumes() {
    let mut fx = setup_with(Tier::Standard, Engine::Claude, Some(300), |l| {
        l.conversation_id = Some("ledger-X".into());
    });
    fx.sched.set_registry_seed(Some("seed-U"));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("ledger-X")
    );
    assert!(launched_flag(&fx, &sess, "--session-id").is_none());
}

#[test]
fn codex_launches_fresh_interactive_without_a_pinned_id() {
    let mut fx = setup(Tier::Standard, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let argv = launched_argv(&fx, &sess).unwrap();
    assert!(argv.iter().any(|a| a == "codex"), "{argv:?}");
    assert!(
        !argv.iter().any(|a| a == "resume"),
        "no id ⇒ fresh: {argv:?}"
    );
    assert!(!argv.iter().any(|a| a == "--session-id"), "{argv:?}");
    assert!(ledger(&fx).conversation_id.is_none());
}

#[test]
fn codex_adopts_the_id_its_own_turn_hook_reported_and_resumes_it() {
    // THE BUG: codex has no caller-chosen id, so `mint_or_fresh` launched it fresh and promised
    // the id would be "captured later" — but nothing captured it. Every relaunch (a crash, a pmd
    // restart, `r`) therefore opened a NEW conversation and orphaned the history. The turn hook's
    // `thread_id` is the only thing that could have said otherwise.
    let captured = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    let mut fx = setup(Tier::Standard, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    std::fs::create_dir_all(fx.paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.codex_conversation_id(), captured).unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let argv = launched_argv(&fx, &sess).unwrap();
    let at = argv.iter().position(|a| a == "resume").expect("resumes");
    assert_eq!(argv.get(at + 1).map(String::as_str), Some(captured));
    // Adopted onto the ledger pmd alone writes, so the NEXT relaunch takes the ledger branch
    // instead of reading the file again.
    assert_eq!(ledger(&fx).conversation_id.as_deref(), Some(captured));
    // CONFIRMED, left at its default. Marking a hook-reported id unconfirmed (as an adopted seed
    // is) would make a later relaunch CREATE instead of resume, and for codex a create is a
    // brand-new conversation — the very orphaning this change exists to prevent. The evidence is
    // stronger than a seed's, too: the id comes from a turn this session COMPLETED, where a seed is
    // only a human's claim that a conversation exists.
    assert!(!ledger(&fx).resume_unconfirmed);
}

#[test]
fn a_second_relaunch_still_resumes_the_same_codex_conversation() {
    // Restart twice. A relaunch that quietly starts a new conversation reads as success in every
    // other assertion — the session is up, it has just lost everything it knew.
    //
    // Each round asserts a NEW launch happened before reading its argv. `launched_argv` returns the
    // LAST launch for the session, so a round that failed to relaunch would otherwise re-inspect
    // the previous round's command and pass. A first version of this test did exactly that, and a
    // mutation run is what exposed it.
    let captured = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    let mut fx = setup(Tier::Standard, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    std::fs::create_dir_all(fx.paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.codex_conversation_id(), captured).unwrap();

    for round in 1..=2i64 {
        fx.driver.set_alive(&sess, false);
        fx.clock.set(START + LAUNCH_GRACE_S * round);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert_eq!(
            fx.driver.launched().len(),
            round as usize,
            "round {round} must actually relaunch"
        );
        let argv = launched_argv(&fx, &sess).unwrap();
        let at = argv
            .iter()
            .position(|a| a == "resume")
            .unwrap_or_else(|| panic!("relaunch {round} must resume: {argv:?}"));
        assert_eq!(
            argv.get(at + 1).map(String::as_str),
            Some(captured),
            "relaunch {round} resumes the same conversation"
        );
    }
}

#[test]
fn a_live_codex_session_adopts_its_reported_id_without_waiting_for_a_relaunch() {
    // Reading the hook only at relaunch left the ledger empty for the whole life of a live session
    // — a WINDOW, not just a delay: a worker-authored id could land first and win permanently, and
    // nothing authoritative existed to compare it against. An ordinary tick reconciles it, with no
    // relaunch involved.
    let captured = "0199dddd-1111-7aaa-8bbb-ccccdddddddd";
    let fx = setup_with(Tier::Standard, Engine::Codex, Some(300), |l| {
        l.conversation_id = None;
        l.run = JobRun::Monitoring { until: START };
    });
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true);
    let mut fx = fx;
    std::fs::create_dir_all(fx.paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.codex_conversation_id(), captured).unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert_eq!(
        ledger(&fx).conversation_id.as_deref(),
        Some(captured),
        "the live session's identity is reconciled on an ordinary tick"
    );
    assert!(
        fx.driver.launched().is_empty(),
        "and nothing was relaunched to get it"
    );
}

#[test]
fn codex_ignores_an_unusable_recorded_id_and_launches_fresh() {
    // A truncated or tampered file must not reach the command line: `codex resume <garbage>` is a
    // launch that dies and takes the whole heartbeat with it. Fresh is the safe reading.
    let mut fx = setup(Tier::Standard, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    std::fs::create_dir_all(fx.paths.codex_conversation_id().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.codex_conversation_id(), "--help").unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    let argv = launched_argv(&fx, &sess).unwrap();
    assert!(!argv.iter().any(|a| a == "resume"), "{argv:?}");
    assert!(ledger(&fx).conversation_id.is_none());
}

#[test]
fn codex_launch_leaves_directory_trust_for_the_human() {
    let mut fx = setup(Tier::Standard, Engine::Codex, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    let argv = launched_argv(&fx, &sess).unwrap();
    assert!(!argv.iter().any(|arg| arg == "-p"), "{argv:?}");
}

// --- resume→create fallback (Standard→autopilot without chatting) --------

#[test]
fn unconfirmed_seed_relaunch_falls_back_to_create_then_resumes_one_shot() {
    // The bug: a human creates a Standard session and flips to Autopilot WITHOUT chatting
    // first, so the adopted registry seed names a conversation claude never persisted.
    // First launch RESUMES the seed (it MIGHT be a real chat conversation); claude prints
    // "No conversation found" and exits; the relaunch must fall back to CREATE
    // (`--session-id`) exactly once instead of looping `--resume <ghost>` forever.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    fx.sched.set_registry_seed(Some("seed-unpersisted"));
    let sess = loop_session(&fx);

    // Tick A: cold launch adopts the seed → RESUME, marking it unconfirmed.
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("seed-unpersisted"),
        "first attempt resumes the seed (preserve chat context if it exists)"
    );
    assert!(launched_flag(&fx, &sess, "--session-id").is_none());
    assert!(
        ledger(&fx).resume_unconfirmed,
        "adopting a seed leaves it unconfirmed until proven"
    );

    // The seed-resume did NOT survive (claude exited: no such conversation). Relaunch.
    fx.driver.set_alive(&sess, false);
    fx.clock.set(START + LAUNCH_GRACE_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.driver.launched().len(), 2, "relaunched");
    assert_eq!(
        launched_flag(&fx, &sess, "--session-id").as_deref(),
        Some("seed-unpersisted"),
        "the unconfirmed relaunch CREATES the same id rather than resuming a ghost"
    );
    assert!(
        launched_argv(&fx, &sess)
            .unwrap()
            .iter()
            .all(|a| a != "--resume"),
        "the create-fallback must not also --resume"
    );
    assert!(
        !ledger(&fx).resume_unconfirmed,
        "the create-fallback clears the flag (one-shot)"
    );

    // Tick C: if the pane dies AGAIN, the flag is now clear → RESUME. The whole point is
    // that create is one-shot: it must NOT loop create→create (that is the original bug).
    fx.driver.set_alive(&sess, false);
    fx.clock.set(START + 2 * LAUNCH_GRACE_S);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(fx.driver.launched().len(), 3, "relaunched again");
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("seed-unpersisted"),
        "after the one-shot create-fallback, later relaunches RESUME the now-real conversation"
    );
    assert!(
        launched_argv(&fx, &sess)
            .unwrap()
            .iter()
            .all(|a| a != "--session-id"),
        "no second CREATE — create→create looping is the defect this fixes"
    );
}

#[test]
fn confirmed_ledger_cid_always_resumes_never_re_creates() {
    // A cid the agent has PROVEN real (`resume_unconfirmed == false`, the default and the
    // post-turn state) must ALWAYS resume across a relaunch — never regress
    // create-autopilot or a post-turn restart into re-CREATING a live conversation.
    let mut fx = setup_with(Tier::Autopilot, Engine::Claude, Some(300), |l| {
        l.conversation_id = Some("real-convo".into());
        l.resume_unconfirmed = false;
    });
    let sess = loop_session(&fx);
    // Session not alive → drive relaunches (it was created by a prior run of the daemon).
    fx.driver.set_alive(&sess, false);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("real-convo"),
        "a confirmed cid resumes"
    );
    assert!(
        launched_argv(&fx, &sess)
            .unwrap()
            .iter()
            .all(|a| a != "--session-id"),
        "a confirmed cid is never re-created"
    );
    assert!(!ledger(&fx).resume_unconfirmed);
}

// --- seed existence probe (skip the doomed first resume) -----------------

#[test]
fn claude_project_slug_matches_claudes_transcript_dir_encoding() {
    // Verified against a live ~/.claude/projects store: every non-alphanumeric char → '-'.
    assert_eq!(
        claude_project_slug(Path::new("/workplace/phahng/agent-manager")),
        "-workplace-phahng-agent-manager"
    );
    assert_eq!(
        claude_project_slug(Path::new("/tmp/pmd-repro.q7TK9j")),
        "-tmp-pmd-repro-q7TK9j"
    );
    assert_eq!(
        claude_project_slug(Path::new("/local/home/phahng/.claude/skills")),
        "-local-home-phahng--claude-skills"
    );
}

#[test]
fn seed_without_a_transcript_creates_directly_skipping_the_doomed_resume() {
    // The user's report: create a Standard session, flip to Autopilot WITHOUT chatting. The
    // seed names a conversation claude never persisted. With claude's `projects` dir present
    // (claude HAS run on this machine), the probe is confident the transcript is ABSENT, so
    // the FIRST launch CREATES — no "No conversation found" resume flashes at all.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    fx.sched.set_registry_seed(Some("seed-neverchatted"));
    // claude's projects dir EXISTS (other conversations live here) but this seed's file does not.
    std::fs::create_dir_all(fx.claude_home.join("projects")).unwrap();
    let sess = loop_session(&fx);

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--session-id").as_deref(),
        Some("seed-neverchatted"),
        "a confidently-absent seed is CREATED directly — no doomed --resume first"
    );
    assert!(
        launched_flag(&fx, &sess, "--resume").is_none(),
        "the probe skips the resume that would print 'No conversation found'"
    );
    assert!(
        !ledger(&fx).resume_unconfirmed,
        "a direct create leaves the flag clear (a dead create falls back to resume, never create→create)"
    );
    assert_eq!(
        ledger(&fx).conversation_id.as_deref(),
        Some("seed-neverchatted")
    );
}

#[test]
fn seed_conversation_exists_reads_the_transcript_store() {
    // Direct coverage of the probe's three outcomes (the resolve-level tests can't
    // distinguish Some(true) from None — both resume — so guard the probe itself).
    let fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    // No `projects` dir yet ⇒ can't say confidently ⇒ None (keeps the optimistic fallback).
    assert_eq!(fx.sched.seed_conversation_exists("any-id"), None);
    // `projects` present but this transcript absent ⇒ confidently absent ⇒ Some(false).
    std::fs::create_dir_all(fx.claude_home.join("projects")).unwrap();
    assert_eq!(fx.sched.seed_conversation_exists("ghost-id"), Some(false));
    // The transcript exists under THIS cwd's slug ⇒ Some(true).
    let convo = fx
        .claude_home
        .join("projects")
        .join(claude_project_slug(&fx.paths.root));
    std::fs::create_dir_all(&convo).unwrap();
    std::fs::write(convo.join("real-id.jsonl"), b"{}\n").unwrap();
    assert_eq!(fx.sched.seed_conversation_exists("real-id"), Some(true));
    // A DIFFERENT cwd's slug dir must not count (claude scopes transcripts by cwd).
    assert_eq!(
        fx.sched.seed_conversation_exists("elsewhere-id"),
        Some(false)
    );
}

#[test]
fn seed_conversation_exists_is_none_for_codex() {
    // codex stores rollouts elsewhere — there is no per-id claude transcript to read, so the
    // probe abstains (None) and codex keeps the optimistic-resume state machine.
    let fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    let convo = fx
        .claude_home
        .join("projects")
        .join(claude_project_slug(&fx.paths.root));
    std::fs::create_dir_all(&convo).unwrap();
    std::fs::write(convo.join("x.jsonl"), b"{}\n").unwrap();
    assert_eq!(fx.sched.seed_conversation_exists("x"), None);
}

#[test]
fn seed_with_an_existing_transcript_resumes_not_creates() {
    // The human DID chat into the seed, so claude persisted its transcript. At the resolve
    // level the exists-arm RESUMES (does not take the Some(false) create path), preserving
    // that context, and stays unconfirmed so a resume that turns out dead can still fall
    // back to create once. (The probe's Some(true) result itself is guarded by
    // `seed_conversation_exists_reads_the_transcript_store`.)
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    fx.sched.set_registry_seed(Some("seed-real"));
    let convo_dir = fx
        .claude_home
        .join("projects")
        .join(claude_project_slug(&fx.paths.root));
    std::fs::create_dir_all(&convo_dir).unwrap();
    std::fs::write(convo_dir.join("seed-real.jsonl"), b"{}\n").unwrap();
    let sess = loop_session(&fx);

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert_eq!(
        launched_flag(&fx, &sess, "--resume").as_deref(),
        Some("seed-real"),
        "an existing transcript RESUMES (preserve the human's chat context)"
    );
    assert!(
        launched_flag(&fx, &sess, "--session-id").is_none(),
        "no CREATE when the conversation already exists"
    );
    assert!(
        ledger(&fx).resume_unconfirmed,
        "still unconfirmed so a dead resume can fall back to create once"
    );
}

// --- worker-skill native install (Milestone E, Task 4) -------------------

#[test]
fn ensure_session_installs_the_worker_skill_natively_for_claude() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // JustLaunched
    // The body is canonical under .agents/skills and Claude reads it through its compatibility
    // symlink.
    let canonical = fx.paths.canonical_worker_skill_file();
    let body =
        std::fs::read_to_string(&canonical).expect("skill installed in <work_dir>/.agents/skills");
    assert_eq!(
        body,
        crate::skills::WORKER_SKILL_MD,
        "byte-for-byte the shipped body"
    );
    assert!(
        std::fs::symlink_metadata(fx.paths.claude_project_skill_dir())
            .unwrap()
            .file_type()
            .is_symlink(),
        "Claude's project skill directory is a compatibility symlink"
    );
    assert_eq!(
        std::fs::read_to_string(fx.paths.claude_project_skill_file()).unwrap(),
        crate::skills::WORKER_SKILL_MD
    );
    // NATIVE discovery — the skills dir is NOT add-dir'd (no reliance on add-dir skill loading).
    let argv = launched_argv(&fx, &sess).expect("launch argv recorded");
    let skills_dir = canonical.parent().unwrap().to_string_lossy().into_owned();
    assert!(
        !argv.iter().any(|a| a == &skills_dir),
        "skills dir NOT add-dir'd: {argv:?}"
    );
    // a successful native install => available (lean nudge branch).
    assert!(fx.sched.worker_skill_available());
    // NEVER the user's AGENTS.md.
    assert!(!fx.paths.root.join("AGENTS.md").exists());
}

#[test]
fn ensure_session_installs_the_native_codex_skill() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    fx.sched.tick(&fx.driver, &fx.clock).unwrap(); // JustLaunched
    assert_eq!(
        std::fs::read_to_string(fx.paths.canonical_worker_skill_file()).unwrap(),
        crate::skills::WORKER_SKILL_MD,
        "Codex discovers the native repository skill under .agents/skills"
    );
    assert!(
        fx.sched.worker_skill_available(),
        "a successful native Codex install takes the lean nudge branch"
    );
    // Codex uses .agents/skills and does not need a Claude compatibility link.
    assert!(!fx.paths.claude_project_skill_dir().exists());
    assert!(
        !fx.paths.root.join("AGENTS.md").exists(),
        "pmd never creates the user's AGENTS.md"
    );
}

#[test]
fn claude_install_overrides_a_legacy_skill_directory() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    std::fs::create_dir_all(fx.paths.claude_project_skill_file().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.claude_project_skill_file(), "old skill").unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert_eq!(
        std::fs::read_to_string(fx.paths.canonical_worker_skill_file()).unwrap(),
        crate::skills::WORKER_SKILL_MD
    );
    assert_eq!(
        std::fs::read_to_string(fx.paths.claude_project_skill_file()).unwrap(),
        crate::skills::WORKER_SKILL_MD
    );
    assert!(fx.sched.worker_skill_available());
}

#[test]
fn claude_install_overrides_the_owned_skill_directory_only() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    std::fs::create_dir_all(fx.paths.claude_project_skill_dir()).unwrap();
    let notes = fx.paths.claude_project_skill_dir().join("notes.md");
    std::fs::write(&notes, "keep me").unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert!(!notes.exists());
    assert!(fx.sched.worker_skill_available());
    assert_eq!(
        std::fs::read_to_string(fx.paths.canonical_worker_skill_file()).unwrap(),
        crate::skills::WORKER_SKILL_MD,
        "the canonical copy remains useful to Codex and the fallback"
    );
}

#[test]
fn worker_skill_lands_on_first_adoption_of_an_already_up_pane() {
    // The real-tmux L2 path: pmd finds a pre-launched pane `AlreadyUp` on the first sweep and
    // must STILL install the skill (adoption: `alive && !installed`) rather than only on launch.
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.driver.set_alive(&sess, true); // a pane pmd did not itself launch is already up
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(
        std::fs::read_to_string(fx.paths.canonical_worker_skill_file()).is_ok(),
        "an adopted already-up pane still gets the canonical skill on the first sweep"
    );
    assert!(
        std::fs::symlink_metadata(fx.paths.claude_project_skill_dir())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fx.sched.worker_skill_available());
    assert!(
        fx.driver.launched().is_empty(),
        "adoption must not relaunch the already-up pane"
    );
}

#[test]
fn pmd_ships_the_spawn_skill_only_with_a_launch_that_names_pmtui() {
    for engine in [Engine::Claude, Engine::Codex] {
        // No pmtui beside pmd: the terminal gets no PMTUI_BIN, so no spawn skill either.
        let mut fx = setup(Tier::Standard, engine, Some(300));
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert!(
            !fx.paths.canonical_spawn_skill_file().exists(),
            "{engine:?}"
        );
        assert!(!fx.paths.claude_spawn_skill_dir().exists(), "{engine:?}");

        // pmd names its sibling pmtui: the next launch ships the skill beside the worker skill.
        let sess = loop_session(&fx);
        fx.sched.set_pmtui_bin(Some(PathBuf::from("/opt/am/pmtui")));
        fx.driver.set_alive(&sess, false);
        fx.clock.set(START + LAUNCH_GRACE_S);
        fx.sched.tick(&fx.driver, &fx.clock).unwrap();
        assert_eq!(fx.driver.launched().len(), 2, "{engine:?}: relaunched");
        assert_eq!(
            std::fs::read_to_string(fx.paths.canonical_spawn_skill_file()).unwrap(),
            crate::skills::SPAWN_SKILL_MD,
            "{engine:?}"
        );
        assert_eq!(
            fx.paths.claude_spawn_skill_dir().exists(),
            engine == Engine::Claude,
            "{engine:?}: only Claude needs the compatibility alias"
        );
        assert!(!fx.paths.root.join("AGENTS.md").exists());
    }
}

#[test]
fn pmd_never_ships_the_spawn_skill_into_a_pane_it_only_adopts() {
    // A pane pmd did not launch carries whatever environment its launcher gave it; pmd ships the
    // skill only with a terminal it starts itself (and names pmtui in).
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let sess = loop_session(&fx);
    fx.sched.set_pmtui_bin(Some(PathBuf::from("/opt/am/pmtui")));
    fx.driver.set_alive(&sess, true);
    fx.driver.set_tail(&sess, IDLE_PANE);
    fx.sched.tick(&fx.driver, &fx.clock).unwrap();
    assert!(fx.driver.launched().is_empty());
    assert!(
        fx.sched.worker_skill_available(),
        "the worker skill still lands"
    );
    assert!(!fx.paths.canonical_spawn_skill_file().exists());
}

#[test]
fn a_failed_spawn_skill_install_never_blocks_the_launch() {
    let mut fx = setup(Tier::Autopilot, Engine::Codex, Some(300));
    fx.sched.set_pmtui_bin(Some(PathBuf::from("/opt/am/pmtui")));
    // A regular file where the canonical skill directory belongs.
    std::fs::create_dir_all(fx.paths.canonical_skills_dir()).unwrap();
    std::fs::write(
        fx.paths
            .canonical_skills_dir()
            .join(crate::skills::SPAWN_SKILL_NAME),
        "blocker",
    )
    .unwrap();

    fx.sched.tick(&fx.driver, &fx.clock).unwrap();

    assert_eq!(fx.driver.launched().len(), 1, "the terminal still starts");
    assert!(fx.sched.worker_skill_available());
}

#[test]
fn mint_uuid_v4_is_well_formed_and_unique() {
    let a = mint_uuid_v4();
    let b = mint_uuid_v4();
    assert_ne!(a, b);
    assert_eq!(a.len(), 36);
    let parts: Vec<&str> = a.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12]
    );
    assert!(parts[2].starts_with('4'), "version 4 nibble: {a}");
    assert!(
        matches!(&parts[3][..1], "8" | "9" | "a" | "b"),
        "variant nibble: {a}"
    );
    assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
}

// --- scheduler boundary behavior ---------------------------------------

#[test]
fn worker_skill_override_changes_the_reported_launch_capability() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));

    fx.sched.set_worker_skill_available(true);
    assert!(fx.sched.worker_skill_available());

    fx.sched.set_worker_skill_available(false);
    assert!(!fx.sched.worker_skill_available());
}

#[test]
fn malformed_ledger_error_names_the_session() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    std::fs::write(fx.paths.pmstate(), "{ not json").unwrap();

    let error = fx.sched.tick(&fx.driver, &fx.clock).unwrap_err();

    assert!(
        format!("{error:#}").contains("load agent-loop ledger for bot.one"),
        "{error:#}"
    );
}

#[test]
fn malformed_control_error_names_the_session() {
    let mut fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    std::fs::write(fx.paths.control(), "{ not json").unwrap();

    let error = fx.sched.tick(&fx.driver, &fx.clock).unwrap_err();

    assert!(
        format!("{error:#}").contains("read control for bot.one"),
        "{error:#}"
    );
}

#[test]
fn decision_history_skips_routine_rows_and_rotates_notable_rows() {
    let fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    let path = fx.paths.decisions();

    fx.sched
        .append_decision_md(START, job::DecisionKind::Working, Some("routine"))
        .unwrap();
    assert!(
        !path.exists(),
        "routine decisions must stay out of decisions.md"
    );

    let old = vec![b'x'; DECISIONS_MD_MAX_BYTES as usize];
    std::fs::write(&path, old).unwrap();
    fx.sched
        .append_decision_md(
            START,
            job::DecisionKind::Escalated,
            Some("  needs review  "),
        )
        .unwrap();

    assert_eq!(
        std::fs::metadata(fx.paths.decisions_rotated())
            .unwrap()
            .len(),
        DECISIONS_MD_MAX_BYTES
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "- 1000 pmd escalated: needs review\n"
    );
}

#[test]
fn side_file_errors_identify_the_failed_operation() {
    let blocked = tempfile::tempdir().unwrap();
    let blocked_root = blocked.path().join("blocked-project");
    std::fs::create_dir_all(&blocked_root).unwrap();
    let blocked_sched = JobScheduler::new(
        SESSION_ID.to_owned(),
        blocked_root.clone(),
        SESSION_ID,
        Engine::Claude,
        None,
    );
    let raw_path = blocked_sched.paths.raw_jsonl();
    let decisions_path = blocked_sched.paths.decisions();
    let side_dir = raw_path.parent().unwrap();
    assert_eq!(
        Some(side_dir),
        decisions_path.parent(),
        "side files share the per-session state directory"
    );
    std::fs::create_dir_all(side_dir.parent().unwrap()).unwrap();
    std::fs::write(side_dir, "not a directory").unwrap();

    let raw_create = blocked_sched.append_raw("row", 1024).unwrap_err();
    assert!(
        format!("{raw_create:#}").contains("create_dir_all"),
        "{raw_create:#}"
    );
    let decision_create = blocked_sched
        .append_decision_md(START, job::DecisionKind::Stalled, Some("stuck"))
        .unwrap_err();
    assert!(
        format!("{decision_create:#}").contains("create_dir_all"),
        "{decision_create:#}"
    );

    let open_fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
    std::fs::create_dir(open_fx.paths.raw_jsonl()).unwrap();
    std::fs::create_dir(open_fx.paths.decisions()).unwrap();
    let raw_open = open_fx.sched.append_raw("row", u64::MAX).unwrap_err();
    assert!(format!("{raw_open:#}").contains("open"), "{raw_open:#}");
    let decision_open = open_fx
        .sched
        .append_decision_md(START, job::DecisionKind::Escalated, Some("review"))
        .unwrap_err();
    assert!(
        format!("{decision_open:#}").contains("open"),
        "{decision_open:#}"
    );

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::symlink;

        let append_fx = setup(Tier::Autopilot, Engine::Claude, Some(300));
        symlink("/dev/full", append_fx.paths.raw_jsonl()).unwrap();
        symlink("/dev/full", append_fx.paths.decisions()).unwrap();

        let raw_append = append_fx.sched.append_raw("row", 1024).unwrap_err();
        assert!(
            format!("{raw_append:#}").contains("append"),
            "{raw_append:#}"
        );
        let decision_append = append_fx
            .sched
            .append_decision_md(START, job::DecisionKind::Escalated, Some("review"))
            .unwrap_err();
        assert!(
            format!("{decision_append:#}").contains("append"),
            "{decision_append:#}"
        );
    }
}
