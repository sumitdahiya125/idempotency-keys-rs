use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdempotencyError {
    #[error("missing required Idempotency-Key header")]
    MissingKey,

    #[error("Idempotency-Key value is not valid UTF-8 or violates the length limit")]
    InvalidKey,

    #[error("key was used for a different request body — refusing to replay")]
    KeyConflict,

    #[error("request body could not be read: {0}")]
    BodyRead(String),

    #[error("storage backend error: {0}")]
    Storage(String),
}
