//! The one decision-tone table: the colour each autopilot decision wears, shared by the preview
//! head and the per-session decision lane (`render::decisions`) so the two surfaces cannot colour
//! the same kind differently.
//!
//! The inline preview feed that used to live here — pmd's per-session action log rendered under the
//! `report` section — was removed once the `v` DECISION LANE became the dedicated home for
//! reviewing what autopilot decided. The preview pane now shows only the agent's own output; pmd's
//! decisions live in `v`.

use crate::*;

/// The colour each decision wears — the dashboard's existing vocabulary: mauve for the heartbeat,
/// dim for a withheld beat, teal for the agent's own report, yellow for a question/re-time, red for
/// a stall, blue for an auto-resolved decision, green for a launch.
pub(crate) fn autopilot_tone(kind: &job::AutopilotEventKind) -> Color {
    use job::AutopilotEventKind::*;
    match kind {
        Nudged => agent_manager::theme::brand(),
        Held(_) => agent_manager::theme::rule(),
        Reported(_) => agent_manager::theme::accent(),
        AutoAnswered(_) | SupervisorResolved(_) | Answered(_) => agent_manager::theme::accent_alt(),
        CadenceChanged(_) | Escalated(_) => agent_manager::theme::soft(),
        Stuck(_) => agent_manager::theme::hard(),
        Launched => agent_manager::theme::live(),
    }
}
