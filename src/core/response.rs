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
    /// sender vanished mid-flight, or a request rejected before it started.
    /// It is deliberately distinct from [`Self::cancelled`], which means "the
    /// user said no". Every other backend uses 2 for these cases.
    pub fn other() -> Self
    where
        T: Default,
    {
        Self(PORTAL_OTHER, T::default())
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
}
