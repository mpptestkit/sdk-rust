use std::fmt;

#[derive(Debug)]
pub struct MppFaucetError {
    pub message: String,
    pub address: String,
}

impl fmt::Display for MppFaucetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MppFaucetError {}

#[derive(Debug)]
pub struct MppPaymentError {
    pub message: String,
    pub url: String,
    pub status: u16,
}

impl fmt::Display for MppPaymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MppPaymentError {}

#[derive(Debug)]
pub struct MppTimeoutError {
    pub message: String,
    pub url: String,
    pub timeout_ms: u64,
}

impl fmt::Display for MppTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MppTimeoutError {}

#[derive(Debug)]
pub struct MppNetworkError {
    pub message: String,
    pub network: String,
}

impl fmt::Display for MppNetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MppNetworkError {}

/// Unified error type for all MPP operations.
#[derive(Debug)]
pub enum Error {
    Faucet(MppFaucetError),
    Payment(MppPaymentError),
    Timeout(MppTimeoutError),
    Network(MppNetworkError),
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Faucet(e) => write!(f, "{}", e),
            Error::Payment(e) => write!(f, "{}", e),
            Error::Timeout(e) => write!(f, "{}", e),
            Error::Network(e) => write!(f, "{}", e),
            Error::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for Error {}

impl From<MppFaucetError>  for Error { fn from(e: MppFaucetError)  -> Self { Error::Faucet(e)   } }
impl From<MppPaymentError> for Error { fn from(e: MppPaymentError) -> Self { Error::Payment(e)  } }
impl From<MppTimeoutError> for Error { fn from(e: MppTimeoutError) -> Self { Error::Timeout(e)  } }
impl From<MppNetworkError> for Error { fn from(e: MppNetworkError) -> Self { Error::Network(e)  } }
