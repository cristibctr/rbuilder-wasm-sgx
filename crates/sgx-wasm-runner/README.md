# SGX WASM Runner

This library enables running the wasm-block-builder inside an Intel SGX enclave using the WebAssembly Micro Runtime (WAMR). It provides a secure execution environment for sensitive block building operations.

## Features

- Run the wasm-block-builder inside an Intel SGX enclave
- Memory-safe interface between Rust and the SGX enclave
- Protection from side-channel and memory attacks
- Confidentiality for WASM execution and state

## Requirements

- Intel SGX SDK (v2.8 or later)
- Rust with the `wasm32-wasip1` target installed

## Setup

1. Install the Intel SGX SDK:
   ```bash
   echo 'deb [arch=amd64] https://download.01.org/intel-sgx/sgx_repo/ubuntu focal main' | sudo tee /etc/apt/sources.list.d/intel-sgx.list
   wget -qO - https://download.01.org/intel-sgx/sgx_repo/ubuntu/intel-sgx-deb.key | sudo apt-key add -
   sudo apt update
   sudo apt install libsgx-launch libsgx-urts sgx-aesm-service libsgx-ae-le libsgx-uae-service libsgx-epid libsgx-urts sgx-aesm-service libsgx-ae-le libsgx-uae-service libsgx-urts libsgx-enclave-common libsgx-enclave-common-dev libsgx-dcap-ql libsgx-dcap-default-qpl libsgx-dcap-ql-dev libsgx-quote-ex libsgx-quote-ex-dev libsgx-ae-qve
   ```

2. Build the wasm-block-builder for WASM:
   ```bash
   cd crates/wasm-block-builder
   cargo build --target wasm32-wasip1 --release
   ```

3. Build this library:
   ```bash
   cd crates/sgx-wasm-runner
   export SGX_SDK=/opt/intel/sgxsdk # Adjust as needed
   cargo build --release
   ```

## Usage

The library provides a high-level API to run the wasm-block-builder in an SGX enclave:

```rust
use sgx_wasm_runner::BlockBuilderSgx;
use serde_json::json;

let wasm_path = "../wasm-block-builder/target/wasm32-wasip1/release/wasm_block_builder.wasm";
let block_builder = BlockBuilderSgx::new(wasm_path)?;

let module_info = block_builder.get_module_info()?;
println!("Module info: {}", module_info);

let block_result = block_builder.build_block(&serde_json::to_string(&input_data)?)?;
```

To run the example:

```bash
cargo run --example run_block_builder
```

## How It Works

1. **SGX Initialization**: The library initializes the SGX environment and creates an enclave.

2. **WASM Loading**: The wasm-block-builder WASM module is loaded into the SGX enclave.

3. **Secure Execution**: All WASM execution happens inside the SGX enclave, protecting sensitive data and execution logic.

4. **Memory Management**: The library handles all memory allocation and deallocation inside the enclave.

5. **Function Calling**: Provides a safe interface to call WASM functions from the block builder.

## Security Considerations

- The Intel SGX provides hardware-level protection against memory analysis
- Side-channel attacks are mitigated by the SGX enclave
- The wasm-block-builder execution is isolated from the rest of the system
- Sensitive data like private keys never leaves the enclave
- Input and output data passes through the enclave boundary in a controlled manner

## Architecture

```
┌──────────────────────────────────────────┐
│                                          │
│         Host Application (Rust)          │
│                                          │
└─────────────────┬────────────────────────┘
                  │
                  ▼
┌──────────────────────────────────────────┐
│                                          │
│         SGX WASM Runner Library          │
│                                          │
└─────────────────┬────────────────────────┘
                  │
                  ▼
┌──────────────────────────────────────────┐
│ ┌────────────────┐      Intel SGX        │
│ │                │      Enclave          │
│ │  WAMR Runtime  ├──────────┐            │
│ │                │          │            │
│ └────────────────┘          │            │
│                             ▼            │
│ ┌────────────────────────────────────┐   │
│ │                                    │   │
│ │      wasm-block-builder.wasm       │   │
│ │                                    │   │
│ └────────────────────────────────────┘   │
│                                          │
└──────────────────────────────────────────┘
```

## Implementation Notes

- The WAMR runtime with SGX support is included as a Git submodule at `crates/wasm-micro-runtime`
- Our build system compiles the SGX support directly from the submodule
- The SGX integration is completely separate from the wasm-block-builder implementation
- The wasm-block-builder WASM module remains unchanged and is simply loaded into the SGX enclave

## License

MIT OR Apache-2.0