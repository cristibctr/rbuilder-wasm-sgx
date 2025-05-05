# WASM Block Builder

This is a WebAssembly (WASM) compatible implementation of Ethereum block building functionality. It allows block building to be performed in a WASI environment without requiring filesystem or database access.

## Features

- Pure in-memory block building with no external dependencies
- Support for all Ethereum transaction types (Legacy, EIP-2930, EIP-1559, EIP-4844/Blob)
- Transaction simulation and gas estimation
- Merkle Patricia Trie state root calculation
- Various transaction ordering algorithms (MEV-Gas-Price, Max-Profit)
- WASI-compatible interface with C-style ABI exports

## Building

To build the WASM module:

```bash
# Build for WASM32-WASI target
cargo build --target wasm32-wasi --release
```

The compiled WASM module will be available at `target/wasm32-wasi/release/wasm_block_builder.wasm`.

## API

The WASM module exports the following functions:

### `build_block`

Builds a block from provided transactions and state.

```c
int build_block(
    uint8_t* input_ptr,
    size_t input_len,
    uint8_t* output_ptr,
    size_t* output_len_ptr
);
```

### `estimate_gas`

Estimates gas for a transaction with given state.

```c
int estimate_gas(
    uint8_t* tx_data_ptr,
    size_t tx_data_len,
    uint8_t* state_data_ptr,
    size_t state_data_len,
    uint64_t* result_ptr
);
```

### `calculate_state_root`

Calculates a state root from state changes.

```c
int calculate_state_root(
    uint8_t* changes_ptr,
    size_t changes_len,
    uint8_t* state_ptr,
    size_t state_len,
    uint8_t* result_ptr,
    size_t* result_len_ptr
);
```

### `get_module_info`

Returns information about the module.

```c
int get_module_info(
    uint8_t* output_ptr,
    size_t* output_len_ptr
);
```

## Example Usage

See the example in `examples/demo.rs` for how to use the WASM block builder from a host program.

To run the example (after building the WASM module):

```bash
cargo run --example demo
```

## Input/Output Format

The WASM block builder uses JSON for input and output data. Here's an example block building input structure:

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

## Architecture

The WASM block builder uses a simplified architecture:

1. **State Management**
   - In-memory state representation
   - Efficient caching for frequently accessed data

2. **Transaction Ordering**
   - Sort by gas price, profit, or custom metrics
   - Handle bundle dependencies

3. **Block Construction**
   - Select transactions to maximize target metric
   - Handle gas limits and other constraints

4. **I/O Interface**
   - Binary serialization for compact representation
   - Well-defined data structures for interoperability

## Limitations

As this is an MVP, there are some limitations:

1. No multi-threading support (WASI limitation)
2. Memory usage scales with state size
3. Performance may be slower than native implementations
4. Some advanced features like custom tracers are limited

## Future Work

1. Optimize memory usage for large states
2. Implement more efficient conflict detection and resolution
3. Add support for Ethereum-compatible chains with custom rules
4. Implement more robust state root calculation
5. Add support for host callbacks for expensive operations