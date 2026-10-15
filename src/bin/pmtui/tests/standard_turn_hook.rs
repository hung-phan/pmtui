//! Preserve the provider's hook authority independently of permission to drive its row.

use super::*;

#[test]
fn standard_turn_hook_authority_depends_on_the_engine() {
    if !tmux_available() {
        return;
    }
    for (tier, engine) in [None, Some(Tier::Standard), Some(Tier::Autopilot)]
        .into_iter()
        .flat_map(|tier| {
            [
                Some(Engine::Claude),
                None,
                Some(Engine::Codex),
                Some(Engine::Trae),
            ]
            .map(|engine| (tier, engine))
        })
    {
        let dir = tempfile::tempdir().unwrap();
        let (registry, root) = reg_with_tier(dir.path(), "bot", tier.unwrap_or(Tier::Standard));
        Registry::update(&registry, |registry| registry.projects[0].engine = engine).unwrap();
        let paths = ProjectPaths::for_session(&root, "bot");
        if tier.is_none() {
            std::fs::remove_file(paths.config()).unwrap();
        }
        let mut ledger = job::load(&paths).unwrap().unwrap();
        ledger.engine = engine.unwrap_or(Engine::Claude);
        ledger.run = job::JobRun::Monitoring {
            until: SystemClock.now() + 300,
        };
        ledger.turn_count_at_nudge = Some(1);
        job::save(&paths, &ledger).unwrap();
        state::write_text_atomic(&paths.turn_signal(), ".").unwrap();
        assert_eq!(
            ProjectView::read_agent_loop("bot", &paths, true, SystemClock.now()).agent_working,
            (tier != Some(Tier::Standard)).then_some(true),
            "main already omits hook state for an explicitly Standard row"
        );

        let pane = match engine {
            Some(Engine::Codex) => {
                "Quiet tool call\n› Ask Codex to do anything\n\n  model footer\n"
            }
            Some(Engine::Trae) => "Quiet tool call\n────\n❯ Explain this codebase\n────\n",
            _ => "Quiet tool call\n> \n",
        };
        let pane_file = root.join("pane.txt");
        std::fs::write(&pane_file, pane).unwrap();
        let socket = TmuxSocket::new("pmtui-standard-turn-hook");
        let driver = TmuxDriver::with_socket(socket.name());
        let session = session_name("bot", &root);
        driver
            .launch_interactive(
                &session,
                &root,
                &[
                    "sh".into(),
                    "-c".into(),
                    "cat \"$1\"; exec cat".into(),
                    "pane".into(),
                    pane_file.display().to_string(),
                ],
                &tmux::ManagedEnv::default(),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if driver
                .capture_tail(&session, 20)
                .unwrap()
                .contains(pane.trim())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture never rendered"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut app = loop_app(&registry);
        app.socket = socket.name().to_string();
        app.refresh();
        app.refresh();
        assert_eq!(
            app.projects[0].agent_working,
            Some(
                tier != Some(Tier::Standard)
                    && match engine {
                        None | Some(Engine::Claude) => true,
                        Some(Engine::Codex) => false,
                        Some(Engine::Trae) => tier == Some(Tier::Autopilot),
                    }
            ),
            "{engine:?}/{tier:?}: a retained Claude hook must survive stable captures"
        );

        state::write_text_atomic(&paths.turn_signal(), "..").unwrap();
        app.refresh();
        assert_eq!(app.projects[0].agent_working, Some(false), "{engine:?}");
    }
}
