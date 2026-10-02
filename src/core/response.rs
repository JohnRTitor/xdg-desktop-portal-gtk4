use {serde::Serialize, zbus::zvariant::Type};

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
        if error.is_user_cancellation() {
            Self::cancelled()
        } else {
            Self::other()
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde::Serialize, zbus::zvariant::Type};

    #[derive(Serialize, Type, Default)]
    #[zvariant(signature = "u")]
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
    /// Regression: every caller collapsed both into code 1, so an application
    /// whose dialog failed to appear -- GTK thread gone, request cancelled, GTK
    /// refusing to build a window -- was told a person declined a prompt they
    /// were never shown. The contract's own split is the reference here: 1 is
    /// "the user cancelled the interaction", 2 is "ended in some other way".
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

        assert_eq!(refused.1.0, 0);
        assert_eq!(never_shown.1.0, 0);
    }
}
