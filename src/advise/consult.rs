//! What one consult IS: the harness's question, the options it enumerated, the grant
//! that follows from them, and the size limits that decide whether the question is worth
//! asking a model about at all. Nothing here talks to a supervisor or reads its reply.

/// The most enumerated options the harness will put in front of a supervisor. Past
/// this a "pick one index" question is not a low-stakes decision any more, and the
/// caller falls back to the static note rather than paying for an opinion.
pub(super) const MAX_CONSULT_OPTIONS: usize = 12;

/// Total bytes of DATA (the goal + the question + the options) the harness will paste
/// into a consult prompt. The marker file is written by the worker, so its size is
/// worker-controlled; without a cap a single report could inflate every consult.
///
/// The GOAL counts too, and that is not a detail. `brief.md` is unbounded — a human
/// pastes a design doc into it and every consult in the session carries it — so a goal
/// left out of this budget silently spends `--max-budget-usd` instead, which surfaces as
/// three `Capability` escalations and a latch: the supervisor "failing" for a reason
/// nothing in the escalation text mentions. Counted here, an over-budget consult instead
/// degrades to the static note, which is the same outcome the latch reaches but without
/// the three escalations and without spending anything.
pub(super) const MAX_CONSULT_DATA_BYTES: usize = 8 * 1024;

/// Bytes of the session GOAL a single consult may carry, out of
/// [`MAX_CONSULT_DATA_BYTES`]. Half the budget, so a long-but-not-absurd brief still
/// leaves room for the worker's question rather than making every consult unconsultable.
pub(super) const MAX_GOAL_BYTES: usize = 4 * 1024;

/// Bytes of the projected SITUATION a single consult may carry. Its OWN clamp
/// ([`clamp_situation`]), and deliberately **excluded** from [`MAX_CONSULT_DATA_BYTES`]:
/// the situation is purely ADDITIVE context, so counting it could turn a previously
/// consultable decision unconsultable exactly when the ledger is richest — the sessions
/// with the MOST history would be the MOST likely to lose the consult. Bounding it on its
/// own keeps it additive and never able to disable a consult.
pub(super) const MAX_SITUATION_BYTES: usize = 2 * 1024;

/// Clamp a session goal to [`MAX_GOAL_BYTES`] for use as [`Consult::goal`].
///
/// Truncation is ANNOUNCED rather than silent: a supervisor reading a goal that was cut
/// mid-sentence would otherwise be judging against a mandate the human did not write, and
/// "the goal does not determine this" is exactly the answer it should give when it cannot
/// see the whole goal. Cuts on a char boundary (the brief is human prose, so multi-byte
/// codepoints are ordinary) — never a panicking byte slice.
pub fn clamp_goal(goal: &str) -> String {
    if goal.len() <= MAX_GOAL_BYTES {
        return goal.to_string();
    }
    let mut end = MAX_GOAL_BYTES;
    while end > 0 && !goal.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[the goal was truncated here: it is longer than the {MAX_GOAL_BYTES} bytes a \
         consult may carry, so you are seeing only its beginning. If what you can see does not \
         determine the answer, refuse.]",
        &goal[..end]
    )
}

/// Clamp a projected situation to [`MAX_SITUATION_BYTES`] for use as [`Consult::situation`].
///
/// Mirrors [`clamp_goal`] — keeps the BEGINNING and announces the cut on a char boundary.
/// The projection orders entries NEWEST-FIRST, so keeping the beginning keeps the most
/// decision-relevant recent history and drops the stale tail. Announced, never silent: a
/// supervisor reading a situation cut mid-entry should know it is partial context.
pub fn clamp_situation(situation: &str) -> String {
    if situation.len() <= MAX_SITUATION_BYTES {
        return situation.to_string();
    }
    let mut end = MAX_SITUATION_BYTES;
    while end > 0 && !situation.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[the situation was truncated here: it is longer than the {MAX_SITUATION_BYTES} \
         bytes a consult may carry, so you are seeing only the most recent entries.]",
        &situation[..end]
    )
}

/// Bytes of the standing DIRECTIVE a single consult may carry. Its OWN clamp
/// ([`clamp_directive`]) and, like [`MAX_SITUATION_BYTES`], deliberately EXCLUDED from
/// [`MAX_CONSULT_DATA_BYTES`]: a restrictive directive is purely additive safety context,
/// so counting it could turn a consultable decision unconsultable exactly when a human has
/// most constrained the session. Smaller than the situation — one operating-constraint, not
/// a history.
pub(super) const MAX_DIRECTIVE_BYTES: usize = 1024;

/// Clamp a standing directive to [`MAX_DIRECTIVE_BYTES`] for use as [`Consult::directive`].
/// Mirrors [`clamp_situation`]: keeps the beginning, announces the cut on a char boundary.
pub fn clamp_directive(directive: &str) -> String {
    if directive.len() <= MAX_DIRECTIVE_BYTES {
        return directive.to_string();
    }
    let mut end = MAX_DIRECTIVE_BYTES;
    while end > 0 && !directive.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[the directive was truncated here: it is longer than the {MAX_DIRECTIVE_BYTES} \
         bytes a consult may carry, so you are seeing only its beginning.]",
        &directive[..end]
    )
}

/// What the harness asked about. Built ENTIRELY from harness + worker state; the
/// supervisor contributes nothing to it, which is what lets [`super::Verdict::Select`] carry
/// the harness's own option text rather than the supervisor's transcription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consult {
    /// Per-consult random token the supervisor must echo verbatim. Also seeds the DATA
    /// fence, so worker-controlled text cannot close the fence it is inside.
    pub nonce: String,
    /// The session goal (`brief.md`) — human-written, but still passed as DATA.
    pub goal: String,
    /// The worker's question, verbatim from its `needs-you.json` stop draft.
    pub question: String,
    /// The worker's enumerated choices, verbatim. Empty ⇒ a free-text answer is what
    /// is being asked for.
    pub options: Vec<String>,
    /// The worker's generic effect classification rendered by the harness. It is untrusted
    /// evidence: explicit human-owned effects are filtered before a consult, while unknown axes
    /// tell the decider what it still needs to establish from the concrete question and project.
    pub reported_effect: Option<String>,
    /// A fresh, in-memory projection of the run's recent decider history (the agent's
    /// stated plan, recent auto-decisions, live open stops), fenced as untrusted DATA.
    /// Bounded by [`clamp_situation`] and EXCLUDED from [`Self::is_consultable`]. Empty ⇒
    /// the prompt omits the SITUATION fence and the consult still runs (a thin ledger must
    /// not lose the consult). NEVER persisted — projected at spawn only.
    pub situation: String,
    /// A standing, restrictive-only operating-directive from the human who owns the session,
    /// read fresh from `directive.md` at spawn. Fenced as TRUSTED, high-authority human
    /// context (unlike `situation`/`goal`, which are untrusted DATA) — but it may only
    /// RESTRICT: the supervisor must refuse if a decision would violate it, and must never
    /// read it as permission to approve. Bounded by [`clamp_directive`], EXCLUDED from
    /// [`Self::is_consultable`], empty ⇒ the DIRECTIVE fence is omitted. NEVER persisted.
    pub directive: String,
}

/// What the supervisor is permitted to do for one consult. Derived from the consult
/// itself, never from the reply — so "an action outside the grant" is decidable before
/// the supervisor has said anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// Options were enumerated: the ONLY legal answer is an index into them. Free text
    /// is outside the grant, so a supervisor that wants to say something else must
    /// refuse (and the human gets the question).
    PickOption,
    /// No options were enumerated: one short imperative instruction is the only thing
    /// that could help, so free text is the grant.
    FreeAnswer,
}

impl Consult {
    /// The grant implied by this consult.
    pub fn grant(&self) -> Grant {
        if self.options.is_empty() {
            Grant::FreeAnswer
        } else {
            Grant::PickOption
        }
    }

    /// Whether this decision is even worth consulting about. `false` ⇒ the caller uses
    /// the static note instead of spawning: there is nothing to ask (a blank question
    /// with no options), too many options for "pick one" to be low-stakes, or more
    /// pasted text than belongs in a prompt.
    ///
    /// The byte budget spans the GOAL as well as the worker's question and options — see
    /// [`MAX_CONSULT_DATA_BYTES`] for why leaving the goal out spent `--max-budget-usd`
    /// instead. Callers are expected to have passed the goal through [`clamp_goal`]
    /// first, so this bound is normally reached only by a genuinely enormous marker.
    pub fn is_consultable(&self) -> bool {
        if self.question.trim().is_empty() && self.options.is_empty() {
            return false;
        }
        if self.options.len() > MAX_CONSULT_OPTIONS {
            return false;
        }
        let bytes = self.goal.len()
            + self.question.len()
            + self.reported_effect.as_deref().map(str::len).unwrap_or(0)
            + self.options.iter().map(String::len).sum::<usize>();
        bytes <= MAX_CONSULT_DATA_BYTES
    }
}
