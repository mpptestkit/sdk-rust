//! # mpp-test-sdk
//!
//! Rust SDK for the [Machine Payments Protocol (MPP)](https://mpptestkit.com) —
//! HTTP 402 payment flow on Solana, no external signing libraries required.
//!
//! ## Client quick-start (devnet, zero config)
//!
//! ```no_run
//! use mpp_test_sdk::{create_test_client, TestClientConfig};
//!
//! #[tokio::main]
//! async fn main() {
//!     let client = create_test_client(TestClientConfig::default()).await.unwrap();
//!     println!("Wallet: {}", client.address);
//!
//!     let resp = client.fetch("http://localhost:3001/api/data", None).await.unwrap();
//!     println!("Status: {}", resp.status());
//! }
//! ```
//!
//! ## Server quick-start
//!
//! ```no_run
//! use mpp_test_sdk::{create_test_server, TestServerConfig, ChargeOptions, ChargeResult};
//!
//! let server = create_test_server(TestServerConfig::default()).unwrap();
//! // Use server.charge(...) inside your request handler.
//! ```

pub mod client;
pub mod errors;
pub mod rpc;
pub mod server;

pub use client::{
    create_test_client, mpp_fetch, reset_mpp_fetch, FetchOptions, PaymentStep, PaymentStepType,
    SolanaNetwork, TestClient, TestClientConfig,
};
pub use errors::{Error, MppFaucetError, MppNetworkError, MppPaymentError, MppTimeoutError};
pub use rpc::LAMPORTS_PER_SOL;
pub use server::{
    create_test_server, ChargeOptions, ChargeResult, MppServer, TestServerConfig,
};
