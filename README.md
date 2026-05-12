# mpp-test-sdk (Rust)

Rust SDK for the **Machine Payments Protocol (MPP)** — HTTP 402 payment flow on Solana.

No external Solana wallet libraries needed. Pure Rust: ed25519 signing, base58, and JSON-RPC over standard async HTTP.

```toml
[dependencies]
mpp-test-sdk = "1.0"
tokio = { version = "1", features = ["full"] }
```

---

## Client

```rust
use mpp_test_sdk::{create_test_client, TestClientConfig};

#[tokio::main]
async fn main() {
    // Zero config — generates wallet, airdrops 2 SOL on devnet
    let client = create_test_client(TestClientConfig::default()).await.unwrap();
    println!("Wallet: {}", client.address);

    let resp = client.fetch("http://localhost:3001/api/data", None).await.unwrap();
    println!("Status: {}", resp.status());
}
```

### Mainnet

```rust
use mpp_test_sdk::{create_test_client, SolanaNetwork, TestClientConfig};

let client = create_test_client(TestClientConfig {
    network: Some(SolanaNetwork::Mainnet),
    secret_key: Some(my_keypair_bytes), // 32-byte seed or 64-byte keypair
    ..Default::default()
})
.await
.unwrap();
```

### Lifecycle callbacks

```rust
use mpp_test_sdk::{create_test_client, TestClientConfig};

let client = create_test_client(TestClientConfig {
    on_step: Some(Box::new(|step| println!("[{:?}] {}", step.step_type, step.message))),
    ..Default::default()
})
.await
.unwrap();
```

### Shared client (drop-in fetch)

```rust
use mpp_test_sdk::mpp_fetch;

let resp = mpp_fetch("http://localhost:3001/api/data", None).await.unwrap();
```

---

## Server

`MppServer::charge` is framework-agnostic — pass the raw `payment-receipt` header value and act on the result.

```rust
use mpp_test_sdk::{create_test_server, ChargeOptions, ChargeResult, TestServerConfig};

let server = create_test_server(TestServerConfig::default()).unwrap();

// Inside your request handler:
let receipt = request.headers().get("payment-receipt").and_then(|v| v.to_str().ok());
match server.charge(receipt, &ChargeOptions { amount: "0.001" }).await {
    ChargeResult::NeedsPayment { payment_request_header, body } => {
        // respond 402, set Payment-Request header
    }
    ChargeResult::Authorized => {
        // serve the response
    }
    ChargeResult::Denied(reason) => {
        // respond 403
    }
}
```

---

## Error types

| Type | When thrown |
|------|-------------|
| `Error::Faucet(MppFaucetError)` | Devnet/testnet airdrop failed after 3 retries |
| `Error::Payment(MppPaymentError)` | HTTP error or malformed `Payment-Request` |
| `Error::Timeout(MppTimeoutError)` | Full flow exceeded timeout (default 30 s) |
| `Error::Network(MppNetworkError)` | Mainnet used without `secret_key` |

---

## Networks

| Constant | RPC | Airdrop |
|----------|-----|---------|
| `SolanaNetwork::Devnet` | api.devnet.solana.com | ✓ |
| `SolanaNetwork::Testnet` | api.testnet.solana.com | ✓ |
| `SolanaNetwork::Mainnet` | api.mainnet-beta.solana.com | — |

---

## Related

- [mpptestkit.com](https://mpptestkit.com) — interactive playground
- [npm: mpp-test-sdk](https://www.npmjs.com/package/mpp-test-sdk) — TypeScript SDK
- [PyPI: mpp-test-sdk](https://pypi.org/project/mpp-test-sdk) — Python SDK
- [GitHub: sdk-go](https://github.com/mpptestkit/sdk-go) — Go SDK
- [GitHub: sdk-rust](https://github.com/mpptestkit/sdk-rust) — this SDK
