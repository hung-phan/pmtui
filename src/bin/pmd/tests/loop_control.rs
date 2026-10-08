use super::*;

fn report(all_enabled_done: bool, idle_expired: bool) -> agent_manager::daemon::SweepReport {
    agent_manager::daemon::SweepReport {
        all_enabled_done,
        idle_expired,
    }
}

#[test]
fn all_done_precedes_idle_expiry() {
    assert_eq!(
        choose_loop_action(&report(true, true)),
        LoopAction::AllEnabledDone
    );
}

#[test]
fn idle_expiry_stops_an_otherwise_active_loop() {
    assert_eq!(
        choose_loop_action(&report(false, true)),
        LoopAction::IdleExpired
    );
}

#[test]
fn active_daemon_without_a_stop_request_continues() {
    assert_eq!(
        choose_loop_action(&report(false, false)),
        LoopAction::Continue
    );
}

#[test]
fn exit_messages_explain_why_the_daemon_stopped() {
    assert_eq!(
        loop_exit_message(LoopAction::StopRequested),
        Some("pmd: stop requested — leaving project terminals running".to_string())
    );
    assert_eq!(
        loop_exit_message(LoopAction::AllEnabledDone),
        Some("pmd: all enabled projects done".to_string())
    );
    assert_eq!(
        loop_exit_message(LoopAction::IdleExpired),
        Some(format!(
            "pmd: nothing to drive for {}s — exiting (a flip will respawn)",
            agent_manager::daemon::IDLE_EXIT_S
        ))
    );
    assert_eq!(loop_exit_message(LoopAction::Continue), None);
}
