use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::errors::{Error, MppFaucetError, MppNetworkError, MppPaymentError, MppTimeoutError};
use crate::rpc::{
    base58_decode, base58_encode, build_transfer_transaction, parse_header_params, RpcClient,
    LAMPORTS_PER_SOL,
};

// ── Network ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolanaNetwork {
    Devnet,
    Testnet,
    Mainnet,
}

impl SolanaNetwork {
    pub fn as_str(&self) -> &'static str {
        match self {
            SolanaNetwork::Devnet => "devnet",
            SolanaNetwork::Testnet => "testnet",
            SolanaNetwork::Mainnet => "mainnet",
        }
    }

    pub fn default_rpc(&self) -> &'static str {
        match self {
            SolanaNetwork::Devnet => "https://api.devnet.solana.com",
            SolanaNetwork::Testnet => "https://api.testnet.solana.com",
            SolanaNetwork::Mainnet => "https://api.mainnet-beta.solana.com",
        }
    }

    pub fn supports_airdrop(&self) -> bool {
        matches!(self, SolanaNetwork::Devnet | SolanaNetwork::Testnet)
    }
}

// ── Lifecycle events ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentStepType {
    WalletCreated,
    Funded,
    Request,
    Payment,
    Retry,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct PaymentStep {
    pub step_type: PaymentStepType,
    pub message: String,
    pub data: HashMap<String, Value>,
}

// ── Config & options ─────────────────────────────────────────────────────────

pub struct TestClientConfig {
    pub network: Option<SolanaNetwork>,
    /// 32-byte seed or 64-byte keypair bytes. Optional on devnet/testnet.
    pub secret_key: Option<Vec<u8>>,
    pub on_step: Option<Box<dyn Fn(PaymentStep) + Send + Sync>>,
    pub timeout: Option<Duration>,
    pub rpc_url: Option<String>,
}

impl Default for TestClientConfig {
    fn default() -> Self {
        Self {
            network: None,
            secret_key: None,
            on_step: None,
            timeout: None,
            rpc_url: None,
        }
    }
}

#[derive(Default)]
pub struct FetchOptions {
    /// HTTP method. Defaults to `"GET"`.
    pub method: Option<String>,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>,
}

// ── TestClient ────────────────────────────────────────────────────────────────

pub struct TestClient {
    pub address: String,
    pub network: SolanaNetwork,
    pub method: String,
    rpc: Arc<RpcClient>,
    signing_key: SigningKey,
    pub_key_bytes: [u8; 32],
    on_step: Arc<dyn Fn(PaymentStep) + Send + Sync>,
    timeout: Duration,
    http: reqwest::Client,
}

impl TestClient {
    /// Performs an HTTP request with automatic MPP 402 payment handling.
    pub async fn fetch(&self, url: &str, opts: Option<FetchOptions>) -> Result<reqwest::Response, Error> {
        let opts = opts.unwrap_or_default();
        let method = opts.method.as_deref().unwrap_or("GET").to_string();

        self.emit(PaymentStepType::Request, format!("→ {url}"), [
            ("url", Value::String(url.to_string())),
        ]);

        // ── Step 1: initial request ───────────────────────────────────
        let resp = tokio::time::timeout(self.timeout, self.do_request(&method, url, &opts.headers, opts.body.as_deref()))
            .await
            .map_err(|_| self.timeout_err(url))?
            .map_err(|e| Error::Other(e.to_string()))?;

        let status = resp.status().as_u16();

        if status != 402 {
            if status < 200 || status >= 300 {
                self.emit(PaymentStepType::Error, format!("← {status}"), [
                    ("status", Value::Number(status.into())),
                ]);
                return Err(Error::Payment(MppPaymentError {
                    message: format!("Payment failed for {url} (HTTP {status})"),
                    url: url.to_string(),
                    status,
                }));
            }
            self.emit(PaymentStepType::Success, format!("← {status} OK"), [
                ("status", Value::Number(status.into())),
            ]);
            return Ok(resp);
        }

        // ── Step 2: parse Payment-Request ─────────────────────────────
        let pr_header = resp
            .headers()
            .get("payment-request")
            .or_else(|| resp.headers().get("Payment-Request"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Payment(MppPaymentError {
                message: format!("Payment failed for {url} (HTTP 402): missing Payment-Request header"),
                url: url.to_string(),
                status: 402,
            }))?;
        drop(resp);

        let params = parse_header_params(&pr_header);

        let recipient = params.get("recipient").cloned().ok_or_else(|| Error::Payment(MppPaymentError {
            message: format!("Payment failed for {url} (HTTP 402): Payment-Request missing recipient"),
            url: url.to_string(),
            status: 402,
        }))?;

        let amount_str = params.get("amount").cloned().ok_or_else(|| Error::Payment(MppPaymentError {
            message: format!("Payment failed for {url} (HTTP 402): Payment-Request missing amount"),
            url: url.to_string(),
            status: 402,
        }))?;

        let amount_sol: f64 = amount_str.parse().map_err(|_| Error::Payment(MppPaymentError {
            message: format!("Payment failed for {url} (HTTP 402): invalid amount: {amount_str}"),
            url: url.to_string(),
            status: 402,
        }))?;

        let lamports = (amount_sol * LAMPORTS_PER_SOL as f64).round() as u64;
        let short_recipient = &recipient[..8.min(recipient.len())];

        self.emit(PaymentStepType::Payment, format!("Paying {amount_sol} SOL → {short_recipient}..."), [
            ("amount",    Value::Number(serde_json::Number::from_f64(amount_sol).unwrap_or_else(|| 0.into()))),
            ("recipient", Value::String(recipient.clone())),
        ]);

        // ── Step 3: submit SOL transfer ───────────────────────────────
        let signature = tokio::time::timeout(self.timeout, self.send_payment(&recipient, lamports))
            .await
            .map_err(|_| self.timeout_err(url))?
            .map_err(|e| Error::Other(format!("mpp: payment failed: {e}")))?;

        let short_sig = &signature[..16.min(signature.len())];
        self.emit(PaymentStepType::Payment, format!("Confirmed: {short_sig}..."), [
            ("signature", Value::String(signature.clone())),
            ("amount",    Value::Number(serde_json::Number::from_f64(amount_sol).unwrap_or_else(|| 0.into()))),
        ]);

        // ── Step 4: retry with Payment-Receipt ───────────────────────
        self.emit(PaymentStepType::Retry, "↑ Retrying with payment proof".to_string(), [
            ("signature", Value::String(signature.clone())),
        ]);

        let receipt = format!(
            r#"solana; signature="{signature}"; network="{}"; amount="{amount_sol}""#,
            self.network.as_str()
        );

        let mut retry_headers = opts.headers.clone();
        retry_headers.insert("payment-receipt".to_string(), receipt);

        let retry_resp = tokio::time::timeout(self.timeout, self.do_request(&method, url, &retry_headers, opts.body.as_deref()))
            .await
            .map_err(|_| self.timeout_err(url))?
            .map_err(|e| Error::Other(e.to_string()))?;

        let retry_status = retry_resp.status().as_u16();
        let step = if retry_status >= 200 && retry_status < 300 {
            PaymentStepType::Success
        } else {
            PaymentStepType::Error
        };
        self.emit(step, format!("← {retry_status}"), [
            ("status",    Value::Number(retry_status.into())),
            ("signature", Value::String(signature)),
        ]);

        Ok(retry_resp)
    }

    async fn send_payment(&self, recipient_b58: &str, lamports: u64) -> Result<String, String> {
        let to_bytes = base58_decode(recipient_b58)
            .map_err(|e| format!("send_payment: invalid recipient: {e}"))?;
        if to_bytes.len() != 32 {
            return Err(format!("send_payment: recipient must be 32 bytes, got {}", to_bytes.len()));
        }
        let to_pubkey: [u8; 32] = to_bytes.try_into().unwrap();

        let (blockhash_b58, _) = self.rpc.get_latest_blockhash().await
            .map_err(|e| format!("send_payment: {e}"))?;
        let blockhash_bytes = base58_decode(&blockhash_b58)
            .map_err(|e| format!("send_payment: invalid blockhash: {e}"))?;
        if blockhash_bytes.len() != 32 {
            return Err("send_payment: blockhash must be 32 bytes".to_string());
        }
        let blockhash: [u8; 32] = blockhash_bytes.try_into().unwrap();

        let tx_b64 = build_transfer_transaction(
            &self.signing_key,
            &self.pub_key_bytes,
            &to_pubkey,
            lamports,
            &blockhash,
        );

        let sig = self.rpc.send_transaction(&tx_b64).await
            .map_err(|e| format!("send_payment: sendTransaction: {e}"))?;
        self.rpc.confirm_transaction(&sig).await
            .map_err(|e| format!("send_payment: confirm: {e}"))?;

        Ok(sig)
    }

    async fn do_request(
        &self,
        method: &str,
        url: &str,
        headers: &HashMap<String, String>,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let m = reqwest::Method::from_bytes(method.as_bytes())
            .unwrap_or(reqwest::Method::GET);
        let mut req = self.http.request(m, url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b.to_vec());
        }
        req.send().await
    }

    fn emit<const N: usize>(&self, step_type: PaymentStepType, message: String, data: [(&str, Value); N]) {
        (self.on_step)(PaymentStep {
            step_type,
            message,
            data: data.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        });
    }

    fn timeout_err(&self, url: &str) -> Error {
        Error::Timeout(MppTimeoutError {
            message: format!(
                "Request to {url} timed out after {}ms. Increase the timeout option or check your Solana RPC connection.",
                self.timeout.as_millis()
            ),
            url: url.to_string(),
            timeout_ms: self.timeout.as_millis() as u64,
        })
    }
}

// ── Airdrop helper ────────────────────────────────────────────────────────────

async fn airdrop_with_retry(rpc: &RpcClient, pubkey: &str) -> Result<(), Error> {
    const AIRDROP_LAMPORTS: u64 = 2 * LAMPORTS_PER_SOL;

    for attempt in 0..3usize {
        let ok = async {
            let sig = rpc.request_airdrop(pubkey, AIRDROP_LAMPORTS).await?;
            rpc.confirm_transaction(&sig).await
        }
        .await;

        if ok.is_ok() {
            return Ok(());
        }
        if attempt == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
    }

    Err(Error::Faucet(MppFaucetError {
        message: format!(
            "Failed to airdrop SOL to wallet {pubkey}. \
             The devnet/testnet faucet may be rate-limited. \
             Wait 30s and retry, or pass a pre-funded secret_key to skip airdrop."
        ),
        address: pubkey.to_string(),
    }))
}

// ── create_test_client ────────────────────────────────────────────────────────

/// Creates a new MPP test client backed by a Solana wallet.
///
/// On devnet/testnet the wallet is funded automatically via airdrop (2 SOL).
/// On mainnet a pre-funded `secret_key` must be provided.
///
/// # Example
/// ```no_run
/// use mpp_test_sdk::create_test_client;
///
/// #[tokio::main]
/// async fn main() {
///     let client = create_test_client(Default::default()).await.unwrap();
///     let resp = client.fetch("http://localhost:3001/api/data", None).await.unwrap();
/// }
/// ```
pub async fn create_test_client(config: TestClientConfig) -> Result<TestClient, Error> {
    let network = config.network.unwrap_or(SolanaNetwork::Devnet);
    let timeout = config.timeout.unwrap_or(Duration::from_secs(30));
    let rpc_url = config.rpc_url.unwrap_or_else(|| network.default_rpc().to_string());

    if network == SolanaNetwork::Mainnet && config.secret_key.is_none() {
        return Err(Error::Network(MppNetworkError {
            message: "create_test_client: mainnet requires a pre-funded secret_key. \
                      Airdrop is not available on mainnet."
                .to_string(),
            network: network.as_str().to_string(),
        }));
    }

    let on_step: Arc<dyn Fn(PaymentStep) + Send + Sync> = match config.on_step {
        Some(f) => Arc::new(f),
        None => Arc::new(|_| {}),
    };

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
            n => {
                return Err(Error::Other(format!(
                    "create_test_client: secret_key must be 32 or 64 bytes, got {n}"
                )))
            }
        },
        None => SigningKey::generate(&mut OsRng),
    };

    let pub_key_bytes: [u8; 32] = signing_key.verifying_key().to_bytes();
    let address = base58_encode(&pub_key_bytes);
    let rpc = Arc::new(RpcClient::new(&rpc_url));

    on_step(PaymentStep {
        step_type: PaymentStepType::WalletCreated,
        message: format!("Wallet {address}"),
        data: [
            ("address".to_string(), Value::String(address.clone())),
            ("network".to_string(), Value::String(network.as_str().to_string())),
        ]
        .into(),
    });

    if network.supports_airdrop() {
        airdrop_with_retry(&rpc, &address).await?;
        on_step(PaymentStep {
            step_type: PaymentStepType::Funded,
            message: format!("Wallet funded via {} airdrop (2 SOL)", network.as_str()),
            data: [
                ("network".to_string(), Value::String(network.as_str().to_string())),
                ("amount".to_string(), Value::Number(2.into())),
            ]
            .into(),
        });
    } else {
        on_step(PaymentStep {
            step_type: PaymentStepType::Funded,
            message: "Using pre-funded mainnet wallet".to_string(),
            data: [("network".to_string(), Value::String(network.as_str().to_string()))].into(),
        });
    }

    Ok(TestClient {
        address,
        network,
        method: "solana".to_string(),
        rpc,
        signing_key,
        pub_key_bytes,
        on_step,
        timeout,
        http: reqwest::Client::new(),
    })
}

// ── Shared client (mpp_fetch) ─────────────────────────────────────────────────

static SHARED_CLIENT: OnceLock<Mutex<Option<Arc<TestClient>>>> = OnceLock::new();

fn shared_mutex() -> &'static Mutex<Option<Arc<TestClient>>> {
    SHARED_CLIENT.get_or_init(|| Mutex::new(None))
}

/// Drop-in replacement for `reqwest::get` that automatically handles the
/// Solana MPP 402 payment flow using a lazily-created shared devnet client.
///
/// Call [`reset_mpp_fetch`] to discard the shared wallet and force a new one.
pub async fn mpp_fetch(url: &str, opts: Option<FetchOptions>) -> Result<reqwest::Response, Error> {
    let client = {
        let guard = shared_mutex().lock().await;
        guard.clone()
    };

    let client = match client {
        Some(c) => c,
        None => {
            let new_client = Arc::new(create_test_client(TestClientConfig::default()).await?);
            let mut guard = shared_mutex().lock().await;
            if guard.is_none() {
                *guard = Some(new_client.clone());
            }
            guard.clone().unwrap()
        }
    };

    client.fetch(url, opts).await
}

/// Discards the shared client used by [`mpp_fetch`].
/// The next call will create a fresh wallet and airdrop funds.
pub async fn reset_mpp_fetch() {
    *shared_mutex().lock().await = None;
}
