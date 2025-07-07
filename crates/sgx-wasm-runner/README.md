# SGX WASM Runner

This library provides a secure runtime for executing the `wasm-block-builder` inside Intel SGX enclaves using the WebAssembly Micro Runtime (WAMR). It enables MEV-protected block building by ensuring transaction ordering and execution cannot be tampered with by malicious actors.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Host Application                     │
│                     (rbuilder)                         │
└─────────────────────┬───────────────────────────────────┘
                      │ BlockBuilderSgx API
                      ▼
┌─────────────────────────────────────────────────────────┐
│                SGX WASM Runner                          │
│  ┌─────────────────┐    ┌─────────────────────────────┐ │
│  │  Memory Mgmt    │    │    Function Calling        │ │
│  │   - wbm_alloc   │    │   - build_block()          │ │  
│  │   - wbm_free    │    │   - order_transactions()   │ │
│  │   - chunking    │    │   - get_public_key()       │ │
│  └─────────────────┘    └─────────────────────────────┘ │
└─────────────────────┬───────────────────────────────────┘
                      │ ECALL/OCALL Interface
                      ▼
┌─────────────────────────────────────────────────────────┐
│                Intel SGX Enclave                       │
│  ┌─────────────────────────────────────────────────────┐ │
│  │              WAMR Runtime                           │ │
│  │   ┌─────────────────────────────────────────────┐   │ │
│  │   │          wasm-block-builder.aot             │   │ │
│  │   │                                             │   │ │
│  │   │  • Transaction Ordering (WasiOrderSorter)  │   │ │
│  │   │  • EVM Execution (REVM integration)       │   │ │
│  │   │  • State Management (WasiStateProvider)   │   │ │
│  │   │  • Cryptographic Signing (BlockSigner)    │   │ │
│  │   └─────────────────────────────────────────────┘   │ │
│  └─────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

## Requirements

- Intel SGX SDK v2.8 or later
- Rust toolchain with `wasm32-wasip1` target
- WAMR compiler (`wamrc`) for AOT compilation

## Installation & Setup

### 1. Install Intel SGX SDK

```bash
# Ubuntu/Debian
echo 'deb [arch=amd64] https://download.01.org/intel-sgx/sgx_repo/ubuntu focal main' | sudo tee /etc/apt/sources.list.d/intel-sgx.list
wget -qO - https://download.01.org/intel-sgx/sgx_repo/ubuntu/intel-sgx-deb.key | sudo apt-key add -
sudo apt update
sudo apt install libsgx-launch libsgx-urts sgx-aesm-service libsgx-ae-le libsgx-uae-service libsgx-epid libsgx-dcap-ql libsgx-dcap-default-qpl libsgx-dcap-ql-dev libsgx-quote-ex libsgx-quote-ex-dev libsgx-ae-qve libsgx-enclave-common libsgx-enclave-common-dev

# Set environment
source /opt/intel/sgxsdk/environment
```

### 2. Install WASM Target

```bash
rustup target add wasm32-wasip1
```

### 3. Build WASM Block Builder

```bash
# Build WASM module
cargo build --target wasm32-wasip1 --release --package wasm-block-builder

# Compile to AOT for SGX (requires wamrc)
PROJECT_PATH=$(pwd)
wamrc --size-level=1 -sgx -o $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.aot $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.wasm
```

### 4. Build SGX Runtime

```bash
# This will trigger build.rs which compiles WAMR with SGX support
cargo build --release --package sgx-wasm-runner
```

The build process automatically compiles the WAMR runtime with SGX support. Verify the enclave was built:

```bash
ls -la crates/wasm-micro-runtime/product-mini/platforms/linux-sgx/enclave-sample/enclave.signed.so
```

## Usage

### Basic API

The library provides the `BlockBuilderSgx` struct for secure block building operations:

```rust
use sgx_wasm_runner::BlockBuilderSgx;
use serde_json::json;

// Initialize SGX enclave with AOT WASM module
let wasm_path = "target/wasm32-wasip1/release/wasm_block_builder.aot";
let sgx_builder = BlockBuilderSgx::new(wasm_path)?;

// Get enclave's public key for signature verification
let public_key = sgx_builder.get_public_key()?;
println!("Enclave public key: {}", public_key);

// Get module information
let module_info = sgx_builder.get_module_info()?;
println!("Module capabilities: {}", module_info);

// Build block inside SGX enclave (Legacy Mode)
let block_output = sgx_builder.build_block(&input_json)?;

// Order transactions only (Ordering-Only Mode)  
let ordering_output = sgx_builder.order_transactions(&ordering_input_json)?;
```

### Execution Modes

#### Legacy Mode - Complete Block Building

```rust
// Prepare complete block building input
let input = json!({
    "block_params": { /* block parameters */ },
    "accounts": [ /* account state */ ],
    "storage": [ /* storage state */ ],
    "code": [ /* contract bytecode */ ],
    "transactions": [ /* transactions to include */ ],
    "bundles": [ /* transaction bundles */ ],
    "config": {
        "sorting": "MevGasPrice",
        "discard_txs": true,
        // ... other config
    }
});

// Execute complete block building inside SGX
let result = sgx_builder.build_block(&serde_json::to_string(&input)?)?;
let output: BlockBuilderOutput = serde_json::from_str(&result)?;

// Output includes: header, transactions, receipts, state_diff, signature
```

#### Ordering-Only Mode - Transaction Ordering

```rust
// Prepare ordering input with pre-calculated simulation values
let ordering_input = json!({
    "block_number": 1000000,
    "block_timestamp": 1681234567,
    "base_fee": "0x3b9aca00",
    "gas_limit": 30000000,
    "orders": [
        {
            "id": "tx_1",
            "order_type": "transaction", 
            "coinbase_profit": "1000000000000000",
            "gas_used": 21000,
            "gas_price": "2000000000",
            "order_hash": "0x..."
        }
        // ... more orders
    ]
});

// Get secure transaction ordering from SGX
let result = sgx_builder.order_transactions(&serde_json::to_string(&ordering_input)?)?;
let ordering: SgxOrderingOutput = serde_json::from_str(&result)?;

// Use ordering.ordered_transaction_ids for host execution
```

### Examples

#### Run Example Application

```bash
# Build and run the example (automatically builds WASM if needed)
cargo run --example run_block_builder -- --build-wasm

# Or with specific AOT file
cargo run --example run_block_builder target/wasm32-wasip1/release/wasm_block_builder.aot
```

#### Integration Test

```bash
# Run integration test with SGX
PROJECT_PATH=$(pwd)
PLAYGROUND=1 \
WAMR_ENCLAVE_PATH=$PROJECT_PATH/crates/wasm-micro-runtime/product-mini/platforms/linux-sgx/enclave-sample/enclave.signed.so \
RUSTFLAGS=-Awarnings \
cargo test --package rbuilder --lib integration::simple::tests::test_simple_example --features sgx_integration -- --nocapture
```

## Internal Architecture

### SGX Enclave Lifecycle

1. **Initialization**: Load and verify the SGX enclave (`enclave.signed.so`)
2. **WASM Loading**: Load AOT-compiled WASM module into enclave memory
3. **Runtime Setup**: Initialize WAMR runtime with SGX-specific configurations
4. **Key Generation**: Generate ECDSA key pair for output signing
5. **Ready State**: Enclave ready to process block building requests

### Memory Management

The SGX runner includes sophisticated memory management optimized for the WASM↔SGX boundary:

```rust
// Custom allocators for enclave memory
wbm_alloc(size) -> ptr        // Allocate memory inside enclave
wbm_free(ptr, size)          // Free allocated memory
wbm_get_byte(offset) -> u8   // Read single byte from enclave memory
wbm_set_byte(value, offset)  // Write single byte to enclave memory

// Chunked data transfer for large payloads
wbm_memory_copy(dest, src, len)  // Bulk memory operations
wbm_read_buffer(src, dest, len)  // Read data from enclave
```

### Function Call Flow

```
Host → SGX Runner → SGX Enclave → WAMR → WASM Module
 ↓                                                ↑
Input Serialization                        Output + Signature
 ↓                                                ↑  
Memory Allocation                          Memory Deallocation
 ↓                                                ↑
Chunked Transfer                           Chunked Transfer
 ↓                                                ↑
ECALL Interface                            Return Values
```

### Error Handling

The system includes comprehensive error handling with automatic recovery:

- **Memory Pressure**: Automatic cleanup and retry on allocation failures
- **Transfer Errors**: Chunked retry for large data transfers  
- **SGX Failures**: Graceful degradation with detailed error reporting
- **WASM Errors**: Proper error propagation from enclave to host

## Development

### Building from Source

```bash
# Clone with submodules
git clone --recursive ....

# Build with SGX support
cargo build --package sgx-wasm-runner --release
```