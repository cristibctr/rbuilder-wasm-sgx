# WASM Block Builder

This is a WebAssembly (WASM) compatible implementation of Ethereum block building functionality designed to run inside Intel SGX enclaves for MEV-protected block building. It provides a secure, isolated environment for transaction ordering and execution.

## Architecture

The WASM block builder consists of several key components:

### Core Modules

1. **Builder** (`src/builder/mod.rs`)
   - `WasiBlockBuilder`: Main block construction engine
   - `WasiSimulator`: Transaction execution and state tracking
   - `WasiOrderSorter`: Transaction ordering algorithms

2. **EVM Integration** (`src/evm/mod.rs`)
   - Full REVM integration for Ethereum Virtual Machine execution
   - State tracing for all reads/writes during execution
   - Support for all Ethereum transaction types and EIPs

3. **State Management** (`src/state/`)
   - `WasiStateProvider`: In-memory state representation
   - Complete state diff calculation and tracking
   - Merkle proof generation for verification

4. **Cryptography** (`src/crypto/mod.rs`)
   - ECDSA key generation and management
   - Block output signing for authenticity
   - Secure key storage within SGX enclave

## Building

### Standard WASM Build

```bash
# Build for WASM32-WASIP1 target (required for SGX integration)
cargo build --target wasm32-wasip1 --release
```

### AOT Compilation for SGX

For use with SGX, compile to AOT (Ahead-of-Time) format:

```bash
# First build the WASM module
cargo build --target wasm32-wasip1 --release

# Then compile to AOT using wamrc
PROJECT_PATH=$(pwd)
wamrc --size-level=1 -sgx -o $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.aot $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.wasm
```

The compiled WASM module will be available at:
- WASM: `target/wasm32-wasip1/release/wasm_block_builder.wasm`
- AOT: `target/wasm32-wasip1/release/wasm_block_builder.aot` (for SGX)

## Exported Functions

The WASM module exports the following C-style functions for use by the SGX runtime:

### `build_block`

Performs complete block building including transaction ordering, execution, and signing.

```c
int build_block(
    uint8_t* input_ptr,     // JSON input data (BlockBuilderInput)
    size_t input_len,       // Length of input data
    uint8_t* output_ptr,    // Buffer for JSON output (BlockBuilderOutput)
    size_t* output_len_ptr  // Input: buffer size, Output: actual size
);
```

**Returns**: 0 on success, negative error code on failure

### `order_transactions`

Performs MEV-protected transaction ordering only (for Ordering-Only mode).

```c
int order_transactions(
    uint8_t* input_ptr,     // JSON input data (SgxOrderingInput)  
    size_t input_len,       // Length of input data
    uint8_t* output_ptr,    // Buffer for JSON output (SgxOrderingOutput)
    size_t* output_len_ptr  // Input: buffer size, Output: actual size
);
```

**Returns**: 0 on success, negative error code on failure

### `estimate_gas`

Estimates gas consumption for a transaction.

```c
int estimate_gas(
    uint8_t* tx_data_ptr,    // JSON transaction data
    size_t tx_data_len,      // Length of transaction data
    uint8_t* state_data_ptr, // JSON state data
    size_t state_data_len,   // Length of state data
    uint64_t* result_ptr     // Output: estimated gas
);
```

### `get_public_key`

Retrieves the enclave's public key for signature verification.

```c
int get_public_key(
    uint8_t* output_ptr,    // Buffer for hex-encoded public key
    size_t* output_len_ptr  // Input: buffer size, Output: key length
);
```

### `get_module_info`

Returns module capabilities and version information.

```c
int get_module_info(
    uint8_t* output_ptr,    // Buffer for JSON module info
    size_t* output_len_ptr  // Input: buffer size, Output: actual size
);
```

## Integration with SGX

This WASM module is designed to run inside Intel SGX enclaves via the `sgx-wasm-runner`. The integration provides:

- **Hardware Security**: All execution happens inside SGX enclave
- **MEV Protection**: Transaction ordering cannot be manipulated
- **Cryptographic Attestation**: All outputs are signed by the enclave
- **Memory Safety**: Complete isolation from host system

### Usage in SGX

```rust
use sgx_wasm_runner::BlockBuilderSgx;

// Load WASM module into SGX enclave
let sgx_builder = BlockBuilderSgx::new("path/to/wasm_block_builder.aot")?;

// Get enclave's public key for verification
let public_key = sgx_builder.get_public_key()?;

// Build block inside SGX enclave
let block_output = sgx_builder.build_block(&input_json)?;

// Verify signature authenticity
let verifier = BlockSignatureVerifier::new(&public_key)?;
assert!(verifier.verify(&block_output)?);
```

## Examples

### Basic Usage

See `examples/demo.rs` for standalone WASM usage:

```bash
cargo run --example demo
```

### SGX Integration

See `../sgx-wasm-runner/examples/run_block_builder.rs` for SGX usage:

```bash
cd ../sgx-wasm-runner
cargo run --example run_block_builder
```

## Data Structures

The WASM block builder uses JSON for all input and output data to ensure compatibility across the WASM boundary.

### Block Building Input (`BlockBuilderInput`)

```json
{
  "block_params": {
    "number": 1000000,
    "timestamp": 1681234567,
    "gas_limit": 30000000,
    "base_fee_per_gas": "0x3b9aca00",
    "coinbase": "0x3333333333333333333333333333333333333333",
    "parent_hash": "0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
    "parent_state_root": "0x0000000000000000000000000000000000000000000000000000000000000000"
  },
  "accounts": [...],
  "storage": [...],
  "code": [...],
  "transactions": [...],
  "bundles": [...],
  "config": {
    "discard_txs": true,
    "sorting": "MevGasPrice",
    "failed_order_retries": 1,
    "drop_failed_orders": true,
    "coinbase_payment": false
  }
}
```

### Block Building Output (`BlockBuilderOutput`)

The output includes the complete block data with cryptographic signature:

```json
{
  "header": {
    "number": 1000000,
    "gas_used": 189000,
    "state_root": "0x...",
    "transactions_root": "0x...",
    "receipts_root": "0x...",
    // ... other header fields
  },
  "transactions": ["0x...", "0x..."], // Encoded transactions
  "receipts": [...],                  // Transaction receipts
  "state_diff": {                     // State changes
    "accounts": [...],
    "storage": [...],
    "code": [...]
  },
  "metrics": {
    "tx_count": 5,
    "gas_used": 189000,
    "block_value": "1500000000000000000",
    "build_time_us": 15000
  },
  "signature": "0x..."              // ECDSA signature for verification
}
```

### Transaction Ordering Input (`SgxOrderingInput`)

For Ordering-Only mode, simplified input with pre-calculated simulation values:

```json
{
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
  ]
}
```

## Development