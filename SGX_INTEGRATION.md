# SGX Integration Guide

This guide explains how to build and run rbuilder with Intel SGX support for secure, MEV-protected block building.

## Overview

The SGX integration provides hardware-level security for block building operations by running the WASM block builder inside an Intel SGX enclave. This ensures that transaction ordering and execution cannot be manipulated by malicious actors, providing strong MEV protection guarantees.

## Architecture

The SGX integration consists of three main components:

1. **sgx_wasm_builder.rs** - Integration layer that connects rbuilder to the SGX runtime
2. **wasm-block-builder** - Pure Rust blockchain logic compiled to WASM
3. **sgx-wasm-runner** - SGX runtime that loads and executes WASM modules securely

### Execution Modes

The system supports two execution modes:

#### Legacy Mode
- Complete block building happens inside SGX enclave
- Maximum security but higher performance overhead
- All transaction execution and state updates are protected

#### Ordering-Only Mode (Default)
- Only transaction ordering happens in SGX
- Host performs transaction execution with SGX-verified ordering
- Better performance while maintaining MEV protection

## Prerequisites

### System Requirements
- Intel SGX-capable processor or SGX in simulation mode
- Intel SGX SDK installed
- Linux environment (tested on Ubuntu 20.04+)

### Software Dependencies
- Rust toolchain with `wasm32-wasip1` target
- Intel SGX SDK v2.8 or later
- WAMR (WebAssembly Micro Runtime) with SGX support

## Setup Instructions

### 1. Clone with Submodules

Make sure the wasm-micro-runtime git submodule was cloned together with the rest of the project:

```bash
git clone --recursive ....
# OR if already cloned:
git submodule update --init --recursive
```

### 2. Install Intel SGX SDK

```bash
# Ubuntu/Debian
echo 'deb [arch=amd64] https://download.01.org/intel-sgx/sgx_repo/ubuntu focal main' | sudo tee /etc/apt/sources.list.d/intel-sgx.list
wget -qO - https://download.01.org/intel-sgx/sgx_repo/ubuntu/intel-sgx-deb.key | sudo apt-key add -
sudo apt update
sudo apt install libsgx-launch libsgx-urts sgx-aesm-service libsgx-ae-le libsgx-uae-service libsgx-epid libsgx-dcap-ql libsgx-dcap-default-qpl libsgx-dcap-ql-dev libsgx-quote-ex libsgx-quote-ex-dev libsgx-ae-qve libsgx-enclave-common libsgx-enclave-common-dev

# Set environment variable
source /opt/intel/sgxsdk/environment
```

### 3. Install Rust WASM Target

```bash
rustup target add wasm32-wasip1
```

### 4. Install WAMR Compiler (wamrc)

Compile it manually or download a pre-compiled binary.

```bash
# Clone WAMR repository (if not using submodule)
git clone https://github.com/bytecodealliance/wasm-micro-runtime.git
cd wasm-micro-runtime

# Build wamrc
cd wamr-compiler
mkdir build && cd build
cmake ..
make

# Add to PATH or copy to /usr/local/bin
sudo cp wamrc /usr/local/bin/
```

## Building

### 1. Build rbuilder with SGX Support

```bash
cargo build --package rbuilder --features sgx_integration --release
```

### 2. Compile WASM Block Builder

```bash
cargo build --target wasm32-wasip1 --release --package wasm-block-builder
```

### 3. Compile WASM to AOT (Ahead-of-Time)

```bash
PROJECT_PATH=$(pwd)
wamrc --size-level=1 -sgx -o $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.aot $PROJECT_PATH/target/wasm32-wasip1/release/wasm_block_builder.wasm
```

The build commands above will trigger `sgx-wasm-runner/build.rs`, which compiles WAMR into a dynamic library. Check that the SGX enclave was built successfully:

```bash
ls -la crates/wasm-micro-runtime/product-mini/platforms/linux-sgx/enclave-sample/
# Should contain enclave.signed.so
```

## Running

### Integration Test

Run the SGX integration test to verify everything is working:

```bash
PROJECT_PATH=$(pwd)
PLAYGROUND=1 \
WAMR_ENCLAVE_PATH=$PROJECT_PATH/crates/wasm-micro-runtime/product-mini/platforms/linux-sgx/enclave-sample/enclave.signed.so \
RUSTFLAGS=-Awarnings \
cargo test --package rbuilder --lib integration::simple::tests::test_simple_example --features sgx_integration -- --nocapture
```

### SGX WASM Runner Example

Test the SGX runner directly:

```bash
cd crates/sgx-wasm-runner
cargo run --example run_block_builder -- ../wasm-block-builder/target/wasm32-wasip1/release/wasm_block_builder.aot
```

### Configuration

Add SGX configuration to your rbuilder config file:

```toml
[sgx_config]
wasm_path = "target/wasm32-wasip1/release/wasm_block_builder.aot"
fallback_to_native = true
execution_mode = "OrderingOnlyMode"  # or "Legacy"
```

## Verification

### Signature Verification

The system automatically verifies all SGX outputs:

```rust
// Automatic verification in sgx_wasm_builder.rs
match self.verifier.verify(&output_json) {
    Ok(_) => {
        // Output is authentic and unmodified
        process_verified_output(output)
    },
    Err(e) => {
        // Handle verification failure
        error!("SGX signature verification failed: {}", e);
    }
}
```

### Public Key Retrieval

Get the enclave's public key for external verification:

```rust
let sgx_builder = BlockBuilderSgx::new(&wasm_path)?;
let public_key = sgx_builder.get_public_key()?;
println!("SGX Enclave Public Key: {}", public_key);
```
