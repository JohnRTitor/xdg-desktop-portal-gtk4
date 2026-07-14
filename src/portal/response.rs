use {serde::Serialize, zbus::zvariant::Type};

const PORTAL_SUCCESS: u32 = 0;
const PORTAL_CANCELLED: u32 = 1;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Type;
    use serde::Serialize;

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
