use thiserror::Error;

#[derive(Debug, Error)]
pub enum UiError {
    #[error("Operation could not be started")]
    Closed,
    #[error("Operation was rejected")]
    Rejected,
}
