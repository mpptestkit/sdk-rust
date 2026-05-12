use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde_json::Value;

use crate::client::SolanaNetwork;
use crate::rpc::{base58_encode, parse_header_params, RpcClient, LAMPORTS_PER_SOL};

pub struct TestServerConfig {
    pub network: Option<SolanaNetwork>,
    /// 32-byte seed or 64-byte keypair bytes. Auto-generated when omitted.
    pub secret_key: Option<Vec<u8>>,
    /// Override the recipient address. Defaults to the keypair's public key.
    pub recipient_address: Option<String>,
    pub rpc_url: Option<String>,
}

impl Default for TestServerConfig {
    fn default() -> Self {
        Self {
            network: None,
            secret_key: None,
            recipient_address: None,
            rpc_url: None,
        }
    }
}

pub struct ChargeOptions<'a> {
    /// Required SOL payment, e.g. `"0.001"`.
    pub amount: &'a str,
}

/// Result returned by [`MppServer::charge`].
pub enum ChargeResult {
    /// No payment found — respond with HTTP 402 and include `payment_request_header`.
    NeedsPayment {
        payment_request_header: String,
        body: Value,
    },
    /// Payment verified on-chain — call your handler.
    Authorized,
    /// Payment present but invalid — respond with HTTP 403 and the reason string.
    Denied(String),
}

pub struct MppServer {
    pub recipient_address: String,
    pub network: SolanaNetwork,
    rpc_url: String,
}

impl MppServer {
    /// Checks the incoming `payment_receipt` header and either authorises the
    /// request or returns the appropriate 402/403 payload.
    ///
    /// `receipt_header` is the raw value of the `payment-receipt` request header
    /// (pass `None` or `Some("")` when the header is absent).
    ///
    /// # Example — axum
    /// ```no_run
    /// use axum::{extract::State, http::HeaderMap, response::IntoResponse, Json};
    /// use mpp_test_sdk::{ChargeOptions, ChargeResult, MppServer};
    /// use std::sync::Arc;
    ///
    /// async fn handler(State(srv): State<Arc<MppServer>>, headers: HeaderMap) -> impl IntoResponse {
    ///     let receipt = headers.get("payment-receipt").and_then(|v| v.to_str().ok());
    ///     match srv.charge(receipt, &ChargeOptions { amount: "0.001" }).await {
    ///         ChargeResult::NeedsPayment { payment_request_header, body } => {
    ///             ([(axum::http::header::HeaderName::from_static("payment-request"),
    ///                payment_request_header)], axum::http::StatusCode::PAYMENT_REQUIRED,
    ///                Json(body)).into_response()
    ///         }
    ///         ChargeResult::Denied(reason) => {
    ///             (axum::http::StatusCode::FORBIDDEN, reason).into_response()
    ///         }
    ///         ChargeResult::Authorized => {
    ///             (axum::http::StatusCode::OK, Json(serde_json::json!({"data": "hello"}))).into_response()
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn charge(&self, receipt_header: Option<&str>, opts: &ChargeOptions<'_>) -> ChargeResult {
        let has_receipt = receipt_header.map_or(false, |s| !s.is_empty());

        if !has_receipt {
            let header = format!(
                r#"solana; amount="{}"; recipient="{}"; network="{}""#,
                opts.amount,
                self.recipient_address,
                self.network.as_str(),
            );
            let body = serde_json::json!({
                "error": "Payment Required",
                "payment": {
                    "amount": opts.amount,
                    "currency": "SOL",
                    "recipient": self.recipient_address,
                    "network": self.network.as_str(),
                }
            });
            return ChargeResult::NeedsPayment {
                payment_request_header: header,
                body,
            };
        }

        let required_sol: f64 = match opts.amount.parse() {
            Ok(v) if v > 0.0 => v,
            _ => {
                return ChargeResult::Denied(format!(
                    "server configuration error: invalid amount {:?}",
                    opts.amount
                ))
            }
        };

        match verify_payment(
            &self.rpc_url,
            receipt_header.unwrap(),
            &self.recipient_address,
            required_sol,
        )
        .await
        {
            Ok(()) => ChargeResult::Authorized,
            Err(reason) => ChargeResult::Denied(reason),
        }
    }
}

async fn verify_payment(
    rpc_url: &str,
    receipt_header: &str,
    recipient_address: &str,
    required_sol: f64,
) -> Result<(), String> {
    let params = parse_header_params(receipt_header);

    let signature = params
        .get("signature")
        .cloned()
        .ok_or_else(|| "Payment-Receipt missing signature field".to_string())?;

    let paid: f64 = params
        .get("amount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    if paid < required_sol {
        let claimed = params.get("amount").map(|s| s.as_str()).unwrap_or("0");
        return Err(format!(
            "Insufficient payment: claimed {claimed} SOL, required {required_sol} SOL"
        ));
    }

    let rpc = RpcClient::new(rpc_url);
    let tx = rpc
        .get_transaction(&signature)
        .await
        .map_err(|e| format!("Payment verification failed: {e}"))?
        .ok_or_else(|| "Transaction not found on chain".to_string())?;

    let meta = tx["meta"]
        .as_object()
        .ok_or_else(|| "Payment verification failed: could not read transaction metadata".to_string())?;

    if !tx["meta"]["err"].is_null() {
        return Err("Transaction failed on chain".to_string());
    }

    let account_keys = tx["transaction"]["message"]["accountKeys"]
        .as_array()
        .ok_or_else(|| "Payment verification failed: missing accountKeys".to_string())?;

    let pre_balances = to_f64_vec(&tx["meta"]["preBalances"]);
    let post_balances = to_f64_vec(&tx["meta"]["postBalances"]);

    let idx = account_keys
        .iter()
        .position(|k| extract_address(k).as_deref() == Some(recipient_address))
        .ok_or_else(|| {
            format!(
                "Recipient {}... not found in transaction",
                &recipient_address[..8.min(recipient_address.len())]
            )
        })?;

    if idx >= pre_balances.len() || idx >= post_balances.len() {
        return Err("Payment verification failed: balance arrays too short".to_string());
    }

    let received = (post_balances[idx] - pre_balances[idx]) / LAMPORTS_PER_SOL as f64;
    if received < required_sol {
        return Err(format!(
            "Payment too small: received {received} SOL, required {required_sol} SOL"
        ));
    }

    let _ = meta; // used for null-check above
    Ok(())
}

fn to_f64_vec(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(|n| n.as_f64()).collect())
        .unwrap_or_default()
}

fn extract_address(key: &Value) -> Option<String> {
    match key {
        Value::String(s) => Some(s.clone()),
        Value::Object(m) => m.get("pubkey")?.as_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// Creates an MPP-enabled server middleware factory.
///
/// A keypair is auto-generated when `secret_key` is omitted.
///
/// # Example
/// ```no_run
/// use mpp_test_sdk::create_test_server;
///
/// let server = create_test_server(Default::default()).unwrap();
/// println!("Recipient: {}", server.recipient_address);
/// ```
pub fn create_test_server(config: TestServerConfig) -> Result<MppServer, String> {
    let network = config.network.unwrap_or(SolanaNetwork::Devnet);
    let rpc_url = config.rpc_url.unwrap_or_else(|| network.default_rpc().to_string());

    let recipient_address = if let Some(addr) = config.recipient_address {
        addr
    } else {
        let signing_key = match config.secret_key {
            Some(key) => match key.len() {
                32 => {
                    let seed: [u8; 32] = key.try_into().unwrap();
                    SigningKey::from_bytes(&seed)
                }
                64 => {
                    let seed: [u8; 32] = key[..32].try_into().unwrap();
                    SigningKey::from_bytes(&seed)
                }
                n => return Err(format!("create_test_server: secret_key must be 32 or 64 bytes, got {n}")),
            },
            None => SigningKey::generate(&mut OsRng),
        };
        let pub_key: [u8; 32] = signing_key.verifying_key().to_bytes();
        base58_encode(&pub_key)
    };

    Ok(MppServer {
        recipient_address,
        network,
        rpc_url,
    })
}
