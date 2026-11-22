use {serde::Serialize, zbus::Type};

const PORTAL_SUCCESS: u32 = 0;
const PORTAL_CANCELLED: u32 = 1;
const PORTAL_OTHER: u32 = 2;

#[derive(Serialize, Type)]
pub struct Response<T: Type>(pub u32, pub T);

impl<T: Type> Response<T> {
    pub fn success(t: T) -> Self {
        Self(PORTAL_SUCCESS, t)
    }

    pub fn cancelled() -> Self
    where
        T: Default,
    {
        Self(PORTAL_CANCELLED, T::default())
    }

    /// Response code 2, "other".
    ///
    /// This is what a request that was never actually performed should report:
    /// a request closed by the frontend (`Request.Close`), a request whose
    /// sender vanished mid-flight, a request rejected before it started, or a
    /// dialog that never opened. It is deliberately distinct from
    /// [`Self::cancelled`], which means "the user said no".
    ///
    /// The contract draws that line itself, defining 1 as "the user cancelled the
    /// interaction" and 2 as "the user interaction was ended in some other way"
    /// (`org.freedesktop.portal.Request.xml`, the `Response` signal). Code 1 is
    /// therefore only truthful when a person was actually asked and declined;
    /// reporting it for a dialog that never appeared asserts something we cannot
    /// know.
    pub fn other() -> Self
    where
        T: Default,
    {
        Self(PORTAL_OTHER, T::default())
    }

    /// Report a failed user interaction, choosing the code from *why* it failed.
    ///
    /// A dialog the user actively dismissed is a cancellation. A dialog that
    /// never opened, or was cancelled from underneath by the caller, is not:
    /// no one was asked, so reporting 1 would tell the sandboxed application
    /// that a person declined something they never saw.
    ///
    /// See [`Self::other`] for the contract wording this maps onto.
    pub fn from_ui_error(error: crate::gui::UiError) -> Self
    where
        T: Default,
    {
        // Match the variants explicitly so a new UiError forces a decision
        // here, rather than silently falling into "other". This must agree
        // with `is_user_cancellation()`, which the logging in the portals uses.
        match error {
            crate::gui::UiError::Rejected => Self::cancelled(),
            crate::gui::UiError::Closed
            | crate::gui::UiError::Refused
            | crate::gui::UiError::Failed => Self::other(),
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde::Serialize, zbus::Type};

    #[derive(Serialize, Type, Default)]
    #[zbus(signature = "u")]
    struct DummyResult(u32);

    #[test]
    fn test_response_success_code() {
        let r = Response::success(DummyResult(42));
        assert_eq!(r.0, 0);
        assert_eq!(r.1.0, 42);
    }

    #[test]
    fn test_response_cancelled_code() {
        let r: Response<DummyResult> = Response::cancelled();
        assert_eq!(r.0, 1);
        assert_eq!(r.1.0, 0);
    }

    /// A dialog the user dismissed is a cancellation; one that never opened is
    /// not.
    ///
    /// The contract's split is the reference: 1 is "the user cancelled the
    /// interaction", 2 is "ended in some other way" (GTK thread gone, request
    /// cancelled, GTK refusing to build a window).
    #[test]
    fn a_user_refusal_is_cancelled_but_an_aborted_dialog_is_other() {
        let refused: Response<DummyResult> = Response::from_ui_error(crate::gui::UiError::Rejected);
        assert_eq!(refused.0, 1, "an explicit refusal is a cancellation");

        let never_shown: Response<DummyResult> =
            Response::from_ui_error(crate::gui::UiError::Closed);
        assert_eq!(
            never_shown.0, 2,
            "a dialog that never opened was not refused by anyone"
        );
        let turned_down: Response<DummyResult> =
            Response::from_ui_error(crate::gui::UiError::Refused);
        assert_eq!(
            turned_down.0, 2,
            "a portal refusal without a dialog is not a user cancellation"
        );
        let failed: Response<DummyResult> = Response::from_ui_error(crate::gui::UiError::Failed);
        assert_eq!(
            failed.0, 2,
            "a failed job after approval is not a cancellation"
        );

        assert_eq!(refused.1.0, 0);
        assert_eq!(never_shown.1.0, 0);
        assert_eq!(turned_down.1.0, 0);
        assert_eq!(failed.1.0, 0);
    }

    /// The explicit match above must stay in step with the helper the portals
    /// use for their log levels; a new UiError variant triggers the match but
    /// not this helper, so keep the agreement pinned.
    #[test]
    fn response_codes_agree_with_is_user_cancellation() {
        for error in [
            crate::gui::UiError::Closed,
            crate::gui::UiError::Rejected,
            crate::gui::UiError::Refused,
            crate::gui::UiError::Failed,
        ] {
            let label = format!("{error:?}");
            let is_cancellation = error.is_user_cancellation();
            let code = Response::<DummyResult>::from_ui_error(error).0;
            assert_eq!(code, if is_cancellation { 1 } else { 2 }, "{label}");
        }
    }
}
