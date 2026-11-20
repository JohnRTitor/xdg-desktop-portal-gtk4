use thiserror::Error;

/// Why a dialog did not produce a result.
///
/// The *outcome categories* map onto different portal response codes, so the
/// distinction is load-bearing rather than cosmetic: the contract defines 1 as
/// "the user cancelled the interaction" and 2 as "the user interaction was
/// ended in some other way" (`org.freedesktop.portal.Request.xml`, the
/// `Response` signal). Only a variant that means "someone was asked and said no"
/// may report 1. `Closed`, `Refused` and `Failed` all report 2, but carry
/// different diagnostics: no dialog, a refusal, and a failed submission.
#[derive(Debug, Error)]
pub enum UiError {
    /// The dialog could not be started, or was torn down before it answered:
    /// the GTK thread went away, or the request was cancelled underneath it.
    ///
    /// The user was never asked, so this is "other" (2), not "cancelled" (1).
    #[error("Operation could not be started")]
    Closed,
    /// The user actively refused: Cancel, Deny, or closing the window.
    #[error("Operation was rejected")]
    Rejected,
    /// The portal refused without asking anyone: a foreign token, a token that
    /// became unclaimable before execution, or a job that could not be prepared.
    /// No user was consulted, so this is "other" (2), never a user cancellation.
    #[error("Operation was refused")]
    Refused,
    /// The user approved the dialog, but the print backend failed to submit the
    /// job. This is "other" (2), not a user cancellation.
    #[error("Print job submission failed")]
    Failed,
}

impl UiError {
    /// Whether this outcome means "the user said no".
    ///
    /// Only [`UiError::Rejected`] does. A caller that reported `Closed` as a
    /// user cancellation would tell the sandboxed application that a person
    /// declined a dialog they were never shown.
    pub fn is_user_cancellation(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_refusal_counts_as_a_user_cancellation() {
        assert!(UiError::Rejected.is_user_cancellation());
        assert!(
            !UiError::Closed.is_user_cancellation(),
            "a dialog that never opened was not refused by the user"
        );
        assert!(
            !UiError::Refused.is_user_cancellation(),
            "a refusal without a dialog is not a user cancellation"
        );
        assert!(
            !UiError::Failed.is_user_cancellation(),
            "a failed submission after approval is not a user cancellation"
        );
    }
}
