use crate::backtest::{
    execute::{BacktestBuilderOutput, BlockBacktestValue},
};
use crate::building::builders::BacktestSimulateBlockInput;
use crate::utils::sgx_signature_verifier::BlockSignatureVerifier;
use crate::primitives::SimulatedOrder;
use reth_transaction_pool::PoolTransaction;
use reth_primitives_traits::SignedTransaction;

use crate::primitives::{Order, MempoolTx, Bundle, TransactionSignedEcRecoveredWithBlobs};
use eyre::{eyre, Result};
use sgx_wasm_runner::BlockBuilderSgx;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;
use serde::{Serialize, Deserialize};
use alloy_primitives::{Address, U256, B256, Bytes};
use std::hash::Hash;
use alloy_consensus::Transaction;
use alloy_provider::Provider;
use alloy_rlp::Encodable;
use reth::providers::StateProvider;
use reth_node_api::FullNodeComponents;
use crate::provider;
use crate::provider::StateProviderFactory;

pub struct SgxWasmBacktestAdapter {
    sgx_builder: BlockBuilderSgx,
    public_key: String,
    verifier: BlockSignatureVerifier,
    initialized: AtomicBool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockBuilderInput {
    block_params: BlockParams,
    accounts: Vec<SerializedAccount>,
    storage: Vec<SerializedStorage>,
    code: Vec<SerializedCode>,
    transactions: Vec<SerializedTransaction>,
    bundles: Vec<SerializedBundle>,
    config: BlockBuilderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockParams {
    number: u64,
    timestamp: u64,
    gas_limit: u64,
    base_fee_per_gas: String,
    coinbase: Address,
    parent_hash: B256,
    parent_state_root: B256,
    withdrawals_root: Option<B256>,
    blob_gas_used: Option<u64>,
    excess_blob_gas: Option<u64>,
    parent_beacon_block_root: Option<B256>,
    prev_randao: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedAccount {
    address: Address,
    balance: U256,
    nonce: u64,
    code_hash: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedStorage {
    address: Address,
    slot: B256,
    value: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedCode {
    hash: B256,
    bytecode: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedTransaction {
    hash: B256,
    from: Address,
    to: Option<Address>,
    value: U256,
    gas_limit: u64,
    gas_price: Option<U256>,
    nonce: u64,
    input: Bytes,
    tx_type: String,
    access_list: Vec<AccessListItem>,
    blob_hashes: Vec<B256>,
    max_priority_fee_per_gas: Option<U256>,
    max_fee_per_gas: Option<U256>,
    max_fee_per_blob_gas: Option<U256>,
    versioned_hashes: Vec<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccessListItem {
    address: Address,
    storage_keys: Vec<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedBundle {
    id: String,
    transactions: Vec<SerializedTransaction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockBuilderConfig {
    discard_txs: bool,
    sorting: String,
    failed_tx_retries: u32,
    drop_failed_txs: bool,
    coinbase_payment: bool,
    build_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockBuilderOutput {
    header: SerializedHeader,
    transactions: Vec<Bytes>,
    receipts: Vec<SerializedReceipt>,
    state_diff: SerializedStateDiff,
    state_root: Option<B256>,
    metrics: BlockMetrics,
    signature: Option<Bytes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedHeader {
    parent_hash: B256,
    number: u64,
    timestamp: u64,
    coinbase: Address,
    difficulty: U256,
    gas_limit: u64,
    gas_used: u64,
    base_fee_per_gas: U256,
    extra_data: Bytes,
    state_root: B256,
    transactions_root: B256,
    receipts_root: B256,
    #[serde(with = "serde_bytes_array")]
    logs_bloom: [u8; 256],
    mix_hash: B256,
    withdrawals_root: Option<B256>,
    blob_gas_used: Option<u64>,
    excess_blob_gas: Option<u64>,
    parent_beacon_block_root: Option<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedReceipt {
    tx_type: u8,
    success: bool,
    cumulative_gas_used: u64,
    logs: Vec<SerializedLog>,
    #[serde(with = "serde_bytes_array")]
    logs_bloom: [u8; 256],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedLog {
    address: Address,
    topics: Vec<B256>,
    data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedStateDiff {
    accounts: Vec<SerializedAccountDiff>,
    storage: Vec<SerializedStorageDiff>,
    code: Vec<SerializedCodeDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedAccountDiff {
    address: Address,
    old_balance: Option<U256>,
    new_balance: Option<U256>,
    old_nonce: Option<u64>,
    new_nonce: Option<u64>,
    old_code_hash: Option<B256>,
    new_code_hash: Option<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedStorageDiff {
    address: Address,
    slot: B256,
    old_value: B256,
    new_value: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedCodeDiff {
    hash: B256,
    bytecode: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockMetrics {
    tx_count: usize,
    blob_count: usize,
    gas_used: u64,
    blob_gas_used: Option<u64>,
    block_value: U256,
    build_time_us: u64,
    trace: Option<SerializedBuildTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedBuildTrace {
    sim_time_us: u64,
    finalize_time_us: u64,
    root_hash_time_us: u64,
    ordering_time_us: u64,
    orders_considered: usize,
    orders_included: usize,
    orders_failed: usize,
}

mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde::de::{Error, Visitor};
    use std::fmt;
    use std::marker::PhantomData;

    pub fn serialize<S, const N: usize>(bytes: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let hex = hex::encode(bytes);
            serializer.serialize_str(&hex)
        } else {
            bytes.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<[u8; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor<const N: usize>(PhantomData<[u8; N]>);

        impl<'de, const N: usize> Visitor<'de> for BytesVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a byte array of length {}", N)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                let bytes = hex::decode(v)
                    .map_err(|_| Error::custom("invalid hex string"))?;
                if bytes.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        bytes.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(&bytes);
                Ok(result)
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        v.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(v);
                Ok(result)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut result = [0u8; N];
                for i in 0..N {
                    match seq.next_element()? {
                        Some(v) => result[i] = v,
                        None => return Err(Error::custom(format!(
                            "expected {} bytes, got {}",
                            N, i
                        ))),
                    }
                }
                Ok(result)
            }
        }

        if deserializer.is_human_readable() {
            deserializer.deserialize_str(BytesVisitor(PhantomData))
        } else {
            deserializer.deserialize_bytes(BytesVisitor(PhantomData))
        }
    }
}

impl SgxWasmBacktestAdapter {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Result<Self> {
        let sgx_builder = BlockBuilderSgx::new(wasm_path)
            .map_err(|e| eyre!("Failed to create SGX WASM block builder: {}", e))?;
        
        info!("SGX WASM block builder created successfully");
        let public_key = sgx_builder.get_public_key()
            .map_err(|e| eyre!("Failed to get public key from SGX enclave: {}", e))?;
        
        info!("Got public key from SGX enclave: {}", public_key);
        let verifier = BlockSignatureVerifier::new(&public_key)
            .map_err(|e| eyre!("Failed to create signature verifier: {}", e))?;
        
        Ok(Self {
            sgx_builder,
            public_key,
            verifier,
            initialized: AtomicBool::new(true),
        })
    }
    fn convert_to_wasm_input<P>(&self, input: &BacktestSimulateBlockInput<P>) -> Result<String>
    where
        P: provider::StateProviderFactory,
    {
        let block_params = BlockParams {
            number: input.ctx.evm_env.block_env.number.to::<u64>(),
            timestamp: input.ctx.attributes.timestamp,
            gas_limit: input.ctx.evm_env.block_env.gas_limit.to::<u64>(),
            base_fee_per_gas: format!("0x{:x}", input.ctx.evm_env.block_env.basefee),
            coinbase: input.ctx.attributes.suggested_fee_recipient,
            parent_hash: input.ctx.attributes.parent,
            parent_state_root: B256::default(),
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            prev_randao: input.ctx.attributes.prev_randao,
        };
        let mut accounts = Vec::new();
        let mut storage = Vec::new();
        let mut code = Vec::new();
        let mut processed_addresses = HashSet::new();

        for order in input.sim_orders.iter() {
            match &order.order {
                Order::Tx(tx) => {
                    processed_addresses.insert(tx.tx_with_blobs.signer());

                    if let Some(to) = tx.tx_with_blobs.to() {
                        processed_addresses.insert(to);
                    }
                },
                Order::Bundle(bundle) => {
                    for (tx, _) in bundle.list_txs() {
                        processed_addresses.insert(tx.signer());

                        if let Some(to) = tx.to() {
                            processed_addresses.insert(to);
                        }
                    }
                },
                Order::ShareBundle(share_bundle) => {
                    for (tx, _) in share_bundle.list_txs() {
                        processed_addresses.insert(tx.signer());

                        if let Some(to) = tx.to() {
                            processed_addresses.insert(to);
                        }
                    }
                }
            }
        }

        let state_provider = input.provider.latest()?;

        for &address in &processed_addresses {
            if let Some(account) = state_provider.basic_account(&address)? {
                accounts.push(SerializedAccount {
                    address,
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.bytecode_hash.unwrap_or_default(),
                });

                if account.bytecode_hash.unwrap_or(B256::ZERO) != B256::ZERO {
                    let code_hash = account.bytecode_hash.unwrap_or(B256::ZERO);
                    if let Some(bytecode) = state_provider.bytecode_by_hash(&account.bytecode_hash.unwrap_or(B256::ZERO))? {
                        code.push(SerializedCode {
                            hash: code_hash,
                            bytecode: alloy_primitives::Bytes::default(),
                        });
                    }
                }
            }
        }
        let mut transactions = Vec::new();
        let mut bundles = Vec::new();
        for order in input.sim_orders {
            match &order.order {
                Order::Tx(tx) => {
                    let tx = &tx.tx_with_blobs;

                    let inner_tx = tx.internal_tx_unsecure().transaction();
                    let from = tx.signer();
                    let hash = tx.hash();

                    let serialized_tx = self.serialize_reth_tx(inner_tx, from, hash);
                    transactions.push(serialized_tx);
                },
                Order::Bundle(bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();

                        let serialized_tx = self.serialize_reth_tx(inner_tx, from, hash);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                            id: order.id().to_string(),
                            transactions: bundle_transactions,
                        });
                    }
                },
                Order::ShareBundle(share_bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in share_bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();

                        let serialized_tx = self.serialize_reth_tx(inner_tx, from, hash);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                            id: order.id().to_string(),
                            transactions: bundle_transactions,
                        });
                    }
                }
            }
        }
        let config = BlockBuilderConfig {
            discard_txs: true,
            sorting: "MevGasPrice".to_string(),
            failed_tx_retries: 1,
            drop_failed_txs: true,
            coinbase_payment: false,
            build_timeout_ms: 1000,
        };
        let wasm_input = BlockBuilderInput {
            block_params,
            accounts,
            storage,
            code,
            transactions,
            bundles,
            config,
        };
        let input_json = serde_json::to_string(&wasm_input)
            .map_err(|e| eyre!("Failed to serialize WASM input: {}", e))?;
        
        Ok(input_json)
    }
    fn convert_from_wasm_output(&self, output_json: &str) -> Result<BlockBacktestValue> {
        self.verifier.verify(output_json)
            .map_err(|e| eyre!("Failed to verify signature: {}", e))?;
        let output: BlockBuilderOutput = serde_json::from_str(output_json)
            .map_err(|e| eyre!("Failed to parse WASM output: {}", e))?;
        let trace = output.metrics.trace.as_ref();
        let orders_considered = trace.map(|t| t.orders_considered).unwrap_or(0);
        let orders_included = trace.map(|t| t.orders_included).unwrap_or(0);
        let included_orders = Vec::new();
        let included_order_profits = Vec::new();
        let builder_output = BacktestBuilderOutput {
            orders_included,
            builder_name: "SGX WASM Builder".to_string(),
            our_bid_value: output.metrics.block_value,
            included_orders,
            included_order_profits,
        };
        let backtest_value = BlockBacktestValue {
            block_number: output.header.number,
            winning_bid_value: output.metrics.block_value,
            simulated_orders_count: orders_considered,
            simulated_total_gas: output.metrics.gas_used,
            filtered_orders_blocklist_count: 0,
            simulated_orders_with_refund: 0,
            simulated_refunds_paid: U256::ZERO,
            extra_data: String::new(),
            builder_outputs: vec![builder_output],
        };
        
        Ok(backtest_value)
    }
    fn serialize_reth_tx(&self, tx: &reth_primitives::Transaction, sender: Address, hash: B256) -> SerializedTransaction {
        let tx_type = match tx.tx_type() {
            reth_primitives::TxType::Legacy => "Legacy",
            reth_primitives::TxType::Eip2930 => "AccessList",
            reth_primitives::TxType::Eip1559 => "EIP1559",
            reth_primitives::TxType::Eip4844 => "EIP4844",
            _ => "Unknown",
        };

        let access_list = if let Some(al) = tx.access_list() {
            al.0.iter().map(|item| AccessListItem {
                address: item.address,
                storage_keys: item.storage_keys.clone(),
            }).collect()
        } else {
            Vec::new()
        };

        SerializedTransaction {
            hash,
            from: sender,
            to: tx.to(),
            value: tx.value(),
            gas_limit: tx.gas_limit().try_into().unwrap_or(30_000_000),
            gas_price: tx.gas_price().map(|gp| U256::from(gp)),
            nonce: tx.nonce(),
            input: tx.input().clone(),
            tx_type: tx_type.to_string(),
            access_list,
            blob_hashes: Vec::new(), // Placeholder
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas().map(|fee| U256::from(fee)),
            max_fee_per_gas: Some(U256::from(tx.max_fee_per_gas())),
            max_fee_per_blob_gas: tx.max_fee_per_blob_gas().map(|fee| U256::from(fee)),
            versioned_hashes: Vec::new(), // Placeholder
        }
    }

    pub fn build_block<P>(&self, input: &BacktestSimulateBlockInput<P>) -> Result<BlockBacktestValue>
    where
        P: reth_provider::StateProvider + provider::StateProviderFactory,
    {
        self.build_block_with_provider(input)
    }

    pub fn build_block_with_provider<P>(&self, input: &BacktestSimulateBlockInput<P>) -> Result<BlockBacktestValue>
    where
        P: provider::StateProviderFactory,
    {
        if !self.initialized.load(Ordering::SeqCst) {
            return Err(eyre!("SGX WASM block builder not initialized"));
        }
        
        let start = Instant::now();
        let input_json = self.convert_to_wasm_input::<P>(input)?;
        
        info!("Converted backtest input to WASM format: {} bytes", input_json.len());
        let output_json = self.sgx_builder.build_block(&input_json)
            .map_err(|e| eyre!("Failed to build block in SGX enclave: {}", e))?;
        
        info!("Block built successfully in SGX enclave: {} bytes", output_json.len());
        let result = self.convert_from_wasm_output(&output_json)?;
        
        info!("Block building completed in {:?}", start.elapsed());
        
        Ok(result)
    }
    pub fn get_public_key(&self) -> &str {
        &self.public_key
    }
}

pub struct SgxWasmBacktestAdapterFactory {
    wasm_path: PathBuf,
    adapters: Arc<RwLock<HashMap<String, Arc<SgxWasmBacktestAdapter>>>>,
}

impl SgxWasmBacktestAdapterFactory {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Self {
        Self {
            wasm_path: wasm_path.as_ref().to_path_buf(),
            adapters: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub fn get_or_create_adapter(&self, id: &str) -> Result<Arc<SgxWasmBacktestAdapter>> {
        if let Some(adapter) = self.adapters.read().unwrap().get(id) {
            return Ok(adapter.clone());
        }
        let adapter = SgxWasmBacktestAdapter::new(&self.wasm_path)?;
        let adapter = Arc::new(adapter);
        self.adapters.write().unwrap().insert(id.to_string(), adapter.clone());
        
        Ok(adapter)
    }
}