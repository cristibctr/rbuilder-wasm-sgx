# SGX WASM Block Builder Integration

This document describes how to set up and use the SGX WASM block builder integration in rbuilder.

## Overview

The SGX WASM block builder runs the sensitive block building logic inside an Intel SGX enclave, which provides hardware-based security for critical operations. This protects against various attacks, including:

- Memory analysis attacks
- Privileged software attacks
- Side-channel attacks

The integration works by:
1. Serializing state and transaction data to pass to the enclave
2. Running the block building process inside the SGX-protected WASM environment
3. Signing the output to ensure integrity
4. Verifying the signature before using the built block

## Requirements

- Intel SGX capable hardware
- SGX driver and SDK installed
- Rust with wasm32-wasip1 target
- rbuilder built with the sgx_integration feature

## Building

### 1. Build the WASM module

```bash
# Add the WASM target
rustup target add wasm32-wasip1

# Build the WASM block builder
cargo build --target wasm32-wasip1 --release -p wasm-block-builder
```

This will create `target/wasm32-wasip1/release/wasm_block_builder.wasm`.

### 2. Build rbuilder with SGX integration

```bash
# Build rbuilder with SGX integration
cargo build --release --features sgx_integration
```

## Configuration

Add the SGX WASM builder to your configuration file:

```toml
# Example configuration
[[builders]]
name = "sgx-wasm"
algo = "sgx-wasm-builder"
wasm_path = "./target/wasm32-wasip1/release/wasm_block_builder.wasm"
fallback_to_native = true
```

Parameters:
- `name`: Unique name for the builder
- `algo`: Must be "sgx-wasm-builder"
- `wasm_path`: Path to the compiled WASM module
- `fallback_to_native`: Whether to fall back to native builder if SGX fails

You can also enable the SGX WASM builder as one of your live builders:

```toml
live_builders = ["sgx-wasm", "mp-ordering"]
```

This includes both the SGX builder and a fallback ordering builder.

## Security Considerations

### Key Management

The SGX WASM block builder generates a new ECDSA key pair when initialized. This key is protected by the SGX enclave and used to sign block outputs to prevent tampering.

The current implementation:
- Generates a new key pair on startup
- Keeps the private key secured within the enclave
- Exports the public key for verification

Future improvements will include:
- Key sealing for persistence
- Remote attestation
- Key rotation

### Signature Verification

All blocks built by the SGX WASM builder are cryptographically signed. The rbuilder verifies these signatures before accepting the block to ensure:

1. The block was genuinely built inside the enclave
2. The block hasn't been tampered with after leaving the enclave

## Backtest Integration

The SGX WASM block builder can also be used with the backtest module to analyze historical performance:

```bash
# Run a backtest with the SGX WASM builder
cargo run --features sgx_integration -- backtest run --path /path/to/data --builder sgx-wasm
```

## Troubleshooting

### SGX Initialization Failed

If you see "SGX initialization failed" errors:
- Verify that your hardware supports SGX
- Check that the SGX driver is loaded (`sgx_linux_x64_driver.ko`)
- Ensure the SGX SDK is properly installed
- Check that the AESM service is running (`sudo service aesmd status`)

### WASM Module Loading Failed

If the WASM module fails to load:
- Verify the path to the WASM module is correct
- Ensure the WASM module was built for the correct target (wasm32-wasip1)
- Check that the WASM module has the correct permissions

### Signature Verification Failed

If signature verification fails:
- This could indicate tampering with the block output
- Verify that the public key is correctly retrieved from the enclave
- Check SGX logs for any integrity violations

## Performance Considerations

The SGX WASM block builder has additional overhead compared to native execution:
- SGX enclave transitions
- WASM interpretation overhead
- Serialization/deserialization at the boundary

However, these costs are balanced by the significant security benefits of running inside an SGX enclave.

## Example Commands

```bash
# Build with SGX integration
cargo build --release --features sgx_integration

# Run rbuilder with SGX configuration
./target/release/rbuilder --config config-sgx-example.toml

# Run a backtest with SGX builder
./target/release/rbuilder --features sgx_integration backtest run --path /path/to/data --builder sgx-wasm
```