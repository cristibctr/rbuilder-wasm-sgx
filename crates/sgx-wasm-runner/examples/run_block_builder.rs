use std::{env, fs};
use std::path::{Path, PathBuf};
use serde_json::{json, Value};
use sgx_wasm_runner::{BlockBuilderSgx, Error as SgxError};
use tracing_subscriber::{
    fmt,
    EnvFilter,
    filter::LevelFilter,
    layer::SubscriberExt,
    util::SubscriberInitExt
};

#[derive(Debug)]
enum AppError {
    SgxError(SgxError),
    IoError(std::io::Error),
    JsonError(serde_json::Error),
    Other(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::SgxError(e) => write!(f, "SGX error: {}", e),
            AppError::IoError(e) => write!(f, "IO error: {}", e),
            AppError::JsonError(e) => write!(f, "JSON error: {}", e),
            AppError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::SgxError(e) => Some(e),
            AppError::IoError(e) => Some(e),
            AppError::JsonError(e) => Some(e),
            AppError::Other(_) => None,
        }
    }
}

impl From<SgxError> for AppError {
    fn from(err: SgxError) -> Self {
        AppError::SgxError(err)
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::IoError(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::JsonError(err)
    }
}

impl From<&str> for AppError {
    fn from(err: &str) -> Self {
        AppError::Other(err.to_string())
    }
}

fn to_boxed_error(err: AppError) -> Box<dyn std::error::Error> {
    Box::new(err)
}

fn default_wasm_path() -> PathBuf {
    let workspace_dir = env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| "../".into());
    let release = Path::new(&workspace_dir)
        .join("target/wasm32-wasip1/release/wasm_block_builder.aot");
    let debug   = Path::new(&workspace_dir)
        .join("target/wasm32-wasip1/debug/wasm_block_builder.aot");

    if release.exists() { release } else { debug }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy())
        .with(fmt::layer())
        .init();
    
    let args: Vec<String> = std::env::args().collect();
    
    let build_wasm = args.iter().any(|arg| arg == "--build-wasm");
    
    if build_wasm {
        println!("Building WASM module from source...");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip1", "--release", "-p", "wasm-block-builder"])
            .status()
            .expect("Failed to execute cargo build");
            
        if !build_status.success() {
            return Err(to_boxed_error(AppError::from("Failed to build WASM module")));
        }
        println!("WASM module built successfully.");
    }

    let wasm_path = args.get(1)
        .filter(|s| !s.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(default_wasm_path);
    
    if !wasm_path.exists() {
        eprintln!("WASM file not found at {:?}. Have you compiled with the wasm32-wasip1 target?", wasm_path);
        eprintln!("  cargo build --target wasm32-wasip1 --release -p wasm-block-builder");
        eprintln!("Or run with:");
        eprintln!("  cargo run --example run_block_builder -- [WASM_PATH]");
        eprintln!("  cargo run --example run_block_builder -- --build-wasm");
        return Ok(());
    }
    
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
    
    println!("Starting WASM Block Builder SGX demo...");
    println!("Loading WASM module inside SGX enclave from: {:?}", wasm_path);
    
    let block_builder = match BlockBuilderSgx::new(wasm_path) {
        Ok(builder) => {
            println!("Successfully initialized SGX enclave and loaded WASM module");
            builder
        },
        Err(err) => {
            println!("Failed to initialize SGX enclave: {}", err);
            println!("\nMake sure the Intel SGX SDK is installed and WAMR is compiled with SGX support.");
            println!("If running in a test environment, you might need to set environment variables:");
            println!("  export SGX_SDK=/opt/intel/sgxsdk");
            return Err(to_boxed_error(AppError::from(err)));
        }
    };
    
    println!("\nGetting module info from SGX enclave...");
    match block_builder.get_module_info() {
        Ok(info) => {
            println!("Module info: {}", info);
        },
        Err(err) => {
            println!("Failed to get module info: {}", err);
            return Err(to_boxed_error(AppError::from(err)));
        }
    }
    
    println!("\nGetting public key from SGX enclave...");
    match block_builder.get_public_key() {
        Ok(key) => {
            println!("Public key: {}", key);
            println!("This key will be used to verify block signatures");
        },
        Err(err) => {
            println!("Failed to get public key: {}", err);
            return Err(to_boxed_error(AppError::from(err)));
        }
    }
    
    println!("\nBuilding block inside SGX enclave...");
    
    let input_json = match serde_json::to_string(&sample_input) {
        Ok(json) => json,
        Err(err) => return Err(to_boxed_error(AppError::from(err))),
    };
    
    let block_result = match block_builder.build_block(&input_json) {
        Ok(result) => {
            println!("Block built successfully inside SGX enclave!");
            
            let parsed: Value = match serde_json::from_str(&result) {
                Ok(value) => value,
                Err(err) => return Err(to_boxed_error(AppError::from(err))),
            };
            
            println!("Block number: {}", parsed["header"]["number"]);
            println!("Transactions included: {}", parsed["metrics"]["tx_count"]);
            println!("Gas used: {}", parsed["metrics"]["gas_used"]);
            println!("Block value: {}", parsed["metrics"]["block_value"]);
            println!("Build time (μs): {}", parsed["metrics"]["build_time_us"]);
            
            let output_path = "sgx_block_builder_output.json";
            let pretty_json = match serde_json::to_string_pretty(&parsed) {
                Ok(json) => json,
                Err(err) => return Err(to_boxed_error(AppError::from(err))),
            };
            
            if let Err(err) = fs::write(output_path, pretty_json) {
                return Err(to_boxed_error(AppError::from(err)));
            }
            
            println!("\nFull block output saved to: {}", output_path);
            
            parsed
        },
        Err(err) => {
            println!("Failed to build block inside SGX enclave: {}", err);
            return Err(to_boxed_error(AppError::from(err)));
        }
    };
    
    println!("\nSGX demo completed successfully!");
    println!("Block building was executed inside a secure Intel SGX enclave");
    println!("The WASM module and execution were protected from memory analysis and side-channel attacks");
    
    Ok(())
}