use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn base58_encode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let leading_zeros = bytes.iter().take_while(|&&b| b == 0).count();
    let mut num = BigUint::from_bytes_be(bytes);
    let base = BigUint::from(58u32);
    let mut result = Vec::new();
    while !num.is_zero() {
        let rem = (num.clone() % &base).to_u64().unwrap() as usize;
        num = num / &base;
        result.push(BASE58_ALPHABET[rem]);
    }
    for _ in 0..leading_zeros {
        result.push(BASE58_ALPHABET[0]);
    }
    result.reverse();
    String::from_utf8(result).unwrap()
}

pub fn base58_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let leading_zeros = s.chars().take_while(|&c| c == '1').count();
    let base = BigUint::from(58u32);
    let mut num = BigUint::zero();
    for c in s.chars() {
        let idx = BASE58_ALPHABET
            .iter()
            .position(|&b| b == c as u8)
            .ok_or_else(|| format!("base58_decode: invalid character {:?}", c))?;
        num = num * &base + BigUint::from(idx as u32);
    }
    let decoded = num.to_bytes_be();
    let mut result = vec![0u8; leading_zeros];
    result.extend_from_slice(&decoded);
    Ok(result)
}

fn encode_compact_u16(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else if n < 0x4000 {
        vec![(n & 0x7F) as u8 | 0x80, (n >> 7) as u8]
    } else {
        vec![
            (n & 0x7F) as u8 | 0x80,
            ((n >> 7) & 0x7F) as u8 | 0x80,
            (n >> 14) as u8,
        ]
    }
}

/// Builds and signs a legacy Solana SOL transfer transaction, returned as base64.
///
/// Wire format mirrors the Go SDK's `buildTransferTransaction` exactly.
pub fn build_transfer_transaction(
    signing_key: &SigningKey,
    from_pubkey: &[u8; 32],
    to_pubkey: &[u8; 32],
    lamports: u64,
    blockhash: &[u8; 32],
) -> String {
    let system_program = [0u8; 32];
    let mut msg: Vec<u8> = Vec::new();

    // Header: 1 required signer, 0 read-only signed, 1 read-only unsigned.
    msg.extend_from_slice(&[1, 0, 1]);

    // Account keys: [from, to, system_program].
    msg.extend_from_slice(&encode_compact_u16(3));
    msg.extend_from_slice(from_pubkey);
    msg.extend_from_slice(to_pubkey);
    msg.extend_from_slice(&system_program);

    // Recent blockhash.
    msg.extend_from_slice(blockhash);

    // 1 instruction.
    msg.extend_from_slice(&encode_compact_u16(1));
    msg.push(2); // program_id_index = 2 (system program)
    msg.extend_from_slice(&encode_compact_u16(2));
    msg.extend_from_slice(&[0, 1]); // account indices: from, to

    // Instruction data: SystemInstruction::Transfer (4 bytes) + lamports (8 bytes LE).
    msg.extend_from_slice(&encode_compact_u16(12));
    msg.extend_from_slice(&2u32.to_le_bytes());
    msg.extend_from_slice(&lamports.to_le_bytes());

    let signature = signing_key.sign(&msg);

    let mut tx: Vec<u8> = Vec::new();
    tx.extend_from_slice(&encode_compact_u16(1));
    tx.extend_from_slice(&signature.to_bytes());
    tx.extend_from_slice(&msg);

    BASE64.encode(&tx)
}

/// Parses a structured header value like:
/// `solana; amount="0.001"; recipient="9WzD..."; network="devnet"`
pub fn parse_header_params(header: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for part in header.split(';').skip(1) {
        let part = part.trim();
        if let Some(eq) = part.find('=') {
            let key = part[..eq].trim().to_lowercase();
            let val = part[eq + 1..].trim().trim_matches('"').to_string();
            params.insert(key, val);
        }
    }
    params
}

// ── JSON-RPC client ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: u32,
    method: &'a str,
    params: Vec<Value>,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

pub struct RpcClient {
    endpoint: String,
    client: reqwest::Client,
}

impl RpcClient {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
        }
    }

    async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let body = serde_json::to_string(&RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        })
        .map_err(|e| format!("rpc marshal: {e}"))?;

        let resp = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("rpc http: {e}"))?;

        let rpc_resp: RpcResponse = resp
            .json()
            .await
            .map_err(|e| format!("rpc decode: {e}"))?;

        if let Some(err) = rpc_resp.error {
            return Err(format!("rpc error {}: {}", err.code, err.message));
        }
        rpc_resp.result.ok_or_else(|| "rpc: null result".to_string())
    }

    pub async fn get_latest_blockhash(&self) -> Result<(String, u64), String> {
        let result = self
            .call(
                "getLatestBlockhash",
                vec![serde_json::json!({"commitment": "confirmed"})],
            )
            .await?;
        let blockhash = result["value"]["blockhash"]
            .as_str()
            .ok_or("getLatestBlockhash: missing blockhash")?
            .to_string();
        let last_valid = result["value"]["lastValidBlockHeight"]
            .as_u64()
            .unwrap_or(0);
        Ok((blockhash, last_valid))
    }

    pub async fn request_airdrop(&self, pubkey: &str, lamports: u64) -> Result<String, String> {
        let result = self
            .call(
                "requestAirdrop",
                vec![Value::String(pubkey.to_string()), Value::Number(lamports.into())],
            )
            .await?;
        result
            .as_str()
            .ok_or_else(|| "requestAirdrop: invalid response".to_string())
            .map(|s| s.to_string())
    }

    pub async fn confirm_transaction(&self, sig: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if Instant::now() > deadline {
                return Err(format!("confirmTransaction: timed out waiting for {sig}"));
            }

            let result = self
                .call(
                    "getSignatureStatuses",
                    vec![
                        serde_json::json!([sig]),
                        serde_json::json!({"searchTransactionHistory": true}),
                    ],
                )
                .await?;

            if let Some(statuses) = result["value"].as_array() {
                if let Some(status) = statuses.first() {
                    if !status.is_null() {
                        if !status["err"].is_null() {
                            return Err(format!("transaction {sig} failed on chain"));
                        }
                        let conf = status["confirmationStatus"].as_str().unwrap_or("");
                        if conf == "confirmed" || conf == "finalized" {
                            return Ok(());
                        }
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn send_transaction(&self, tx_base64: &str) -> Result<String, String> {
        let result = self
            .call(
                "sendTransaction",
                vec![
                    Value::String(tx_base64.to_string()),
                    serde_json::json!({"encoding": "base64"}),
                ],
            )
            .await?;
        result
            .as_str()
            .ok_or_else(|| "sendTransaction: invalid response".to_string())
            .map(|s| s.to_string())
    }

    pub async fn get_transaction(&self, sig: &str) -> Result<Option<Value>, String> {
        let result = self
            .call(
                "getTransaction",
                vec![
                    Value::String(sig.to_string()),
                    serde_json::json!({
                        "encoding": "jsonParsed",
                        "commitment": "confirmed",
                        "maxSupportedTransactionVersion": 0,
                    }),
                ],
            )
            .await?;
        if result.is_null() {
            Ok(None)
        } else {
            Ok(Some(result))
        }
    }
}
