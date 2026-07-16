use thiserror::Error;

pub type Result<T> = std::result::Result<T, AcendError>;

#[derive(Debug, Error)]
pub enum AcendError {
    #[error("pair not found: {0}")]
    PairNotFound(String),

    #[error("amount ${0} exceeds pair max ${1}")]
    OverMaxSize(f64, f64),

    #[error("all-in {got:.2} bps exceeds cap {cap:.2} bps vs Pyth mid")]
    OverBpsCap { got: f64, cap: f64 },

    #[error("lending share {got:.1}% below minimum 60%")]
    LendingShareTooLow { got: f64 },

    #[error("oracle: {0}")]
    Oracle(String),

    #[error("config: {0}")]
    Config(String),

    #[error("compose: {0}")]
    Compose(String),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl From<std::io::Error> for AcendError {
    fn from(e: std::io::Error) -> Self {
        AcendError::Config(e.to_string())
    }
}
