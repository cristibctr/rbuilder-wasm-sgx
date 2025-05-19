#![cfg(feature = "sgx_tests")]

use std::path::PathBuf;
use std::time::Duration;
use crate::utils::sgx_signature_verifier::BlockSignatureVerifier;
use eyre::Result;
use serde_json::json;
use tracing::{info, warn};

#[tokio::test]
pub async fn test_sgx_wasm_block_builder() -> Result<()> {
    let wasm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/wasm_block_builder.aot");
    
    info!("Using WASM module at: {:?}", wasm_path);
    if !wasm_path.exists() {
        warn!("WASM module not found at {:?}", wasm_path);
        warn!("Make sure to build it with: cargo build --target wasm32-wasip1 --release -p wasm-block-builder");
        return Ok(());
    }
    let sgx_builder = sgx_wasm_runner::BlockBuilderSgx::new(&wasm_path)?;
    let public_key = sgx_builder.get_public_key()?;
    info!("SGX enclave public key: {}", public_key);
    let verifier = BlockSignatureVerifier::new(&public_key)?;
    let keccak_empty = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470";
    let sample_input = json!({
        "block_params": {
            "number": 1000000,
            "timestamp": 1681234567,
            "gas_limit": 30000000,
            "base_fee_per_gas": "0x3b9aca00",
            "coinbase": "0x3333333333333333333333333333333333333333",
            "parent_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "parent_state_root": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "withdrawals_root": null,
            "blob_gas_used": null,
            "excess_blob_gas": null
        },
        "accounts": [
            {
                "address": "0x1111111111111111111111111111111111111111",
                "balance": "0x3635c9adc5dea00000",
                "nonce": 5,
                "code_hash": keccak_empty
            },
            {
                "address": "0x2222222222222222222222222222222222222222",
                "balance": "0x56bc75e2d63100000",
                "nonce": 0,
                "code_hash": keccak_empty
            },
            {
                "address": "0x3333333333333333333333333333333333333333",
                "balance": "0x0",
                "nonce": 0,
                "code_hash": keccak_empty
            }
        ],
        "storage": [],
        "code": [],
        "transactions": [
            {
                "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                "from": "0x1111111111111111111111111111111111111111",
                "to": "0x2222222222222222222222222222222222222222",
                "value": "0x1bc16d674ec80000",
                "gas_limit": 21000,
                "gas_price": "0x3b9aca00",
                "nonce": 5,
                "input": "0x",
                "tx_type": "Legacy",
                "access_list": [],
                "blob_hashes": [],
                "max_priority_fee_per_gas": null,
                "max_fee_per_blob_gas": null,
                "versioned_hashes": []
            }
        ],
        "bundles": [],
        "config": {
            "discard_txs": true,
            "sorting": "MevGasPrice",
            "failed_tx_retries": 1,
            "drop_failed_txs": true,
            "coinbase_payment": false,
            "build_timeout_ms": 1000
        }
    });
    let input_json = serde_json::to_string(&sample_input)?;
    let start = std::time::Instant::now();
    let output_json = sgx_builder.build_block(&input_json)?;
    
    let build_time = start.elapsed();
    info!("Block built in {:?}", build_time);
    let signature_verified = verifier.verify(&output_json)?;
    info!("Signature verification: {}", signature_verified);
    let output: serde_json::Value = serde_json::from_str(&output_json)?;
    
    info!("Block number: {}", output["header"]["number"]);
    info!("Transactions included: {}", output["metrics"]["tx_count"]);
    info!("Gas used: {}", output["metrics"]["gas_used"]);
    info!("Block value: {}", output["metrics"]["block_value"]);
    info!("Build time (μs): {}", output["metrics"]["build_time_us"]);
    info!("Testing multiple block builds...");
    
    for i in 0..5 {
        let start = std::time::Instant::now();
        let _ = sgx_builder.build_block(&input_json)?;
        info!("Block {} built in {:?}", i, start.elapsed());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    
    info!("SGX WASM block builder integration test completed successfully!");
    
    Ok(())
}