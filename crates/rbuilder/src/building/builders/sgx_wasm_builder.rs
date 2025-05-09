use std::fmt::Debug;
use std::path::PathBuf;

use alloy_primitives::{Address, StorageValue, B256};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error, debug};
use tokio::sync::oneshot;
use std::hash::Hash;
use alloy_consensus::Transaction;
use reth_node_api::{FullNodeComponents, FullNodeTypes};
use reth_provider::{AccountReader, StateProvider, StateProviderFactory};
use reth_transaction_pool::PoolTransaction;
use reth_primitives_traits::SignedTransaction;

use crate::building::{
    builders::{
        BlockBuildingAlgorithm, BlockBuildingAlgorithmInput, BlockBuildingContext,
        block_building_helper::{self, BlockBuildingHelper, BiddableUnfinishedBlock, BlockBuildingHelperError},
        OrderIntakeConsumer,
    },
    BuiltBlockTrace, CriticalCommitOrderError, ExecutionError, ExecutionResult, PrioritizedOrderStore, OrderPriority,
    SimulatedOrderSink,
};
use crate::utils::NonceCache;
use crate::primitives::SimValue;
use std::sync::Arc;
use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub struct ValidOrderPriority {
    order: Arc<crate::primitives::SimulatedOrder>,
}

impl PartialEq for ValidOrderPriority {
    fn eq(&self, other: &Self) -> bool {
        self.order.id() == other.order.id()
    }
}

impl Eq for ValidOrderPriority {}

impl PartialOrd for ValidOrderPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ValidOrderPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order.sim_value.coinbase_profit.cmp(&other.order.sim_value.coinbase_profit)
            .then_with(|| self.order.id().cmp(&other.order.id()))
    }
}

impl OrderPriority for ValidOrderPriority {
    fn new(order: Arc<crate::primitives::SimulatedOrder>) -> Self {
        Self { order }
    }

    fn simulation_too_low(original_sim_value: &SimValue, new_sim_value: &SimValue) -> bool {
        new_sim_value.coinbase_profit * alloy_primitives::U256::from(100) < (original_sim_value.coinbase_profit * alloy_primitives::U256::from(95))
    }
}

type ValidatedOrderMap = PrioritizedOrderStore<ValidOrderPriority>;

impl ValidatedOrderMap {
    pub fn is_empty(&self) -> bool {
        self.get_all_orders().is_empty()
    }

    pub fn get_validated_orders(&self) -> Vec<(String, crate::primitives::Order)> {
        self.get_all_orders()
            .into_iter()
            .map(|sim_order| (sim_order.id().to_string(), sim_order.order.clone()))
            .collect()
    }
}

use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;
use alloy_primitives::U256;
use alloy_provider::Provider;
use alloy_rlp::Encodable;

#[derive(Debug, Clone)]
struct OrderingId(String);

use crate::primitives::Order;

struct MockBlockBuildingHelper {
    true_block_value: U256,
    builder_name: String,
}

impl MockBlockBuildingHelper {
    pub fn new(builder_name: String, true_block_value: U256) -> Self {
        Self {
            true_block_value,
            builder_name,
        }
    }
}

impl block_building_helper::BlockBuildingHelper for MockBlockBuildingHelper {
    fn box_clone(&self) -> Box<dyn block_building_helper::BlockBuildingHelper> {
        Box::new(Self {
            true_block_value: self.true_block_value,
            builder_name: self.builder_name.clone(),
        })
    }

    fn commit_order(
        &mut self,
        _order: &crate::primitives::SimulatedOrder,
        _result_filter: &dyn Fn(&crate::primitives::SimValue) -> Result<(), crate::building::ExecutionError>,
    ) -> Result<Result<&crate::building::ExecutionResult, crate::building::ExecutionError>, crate::building::CriticalCommitOrderError> {
        unimplemented!("Mock implementation does not support commit_order")
    }

    fn set_trace_fill_time(&mut self, _time: std::time::Duration) {
    }

    fn set_trace_orders_closed_at(&mut self, _orders_closed_at: time::OffsetDateTime) {
    }

    fn can_add_payout_tx(&self) -> bool {
        true
    }

    fn true_block_value(&self) -> std::result::Result<U256, BlockBuildingHelperError> {
        Ok(self.true_block_value)
    }

    fn finalize_block(
        self: Box<Self>,
        _payout_tx_value: Option<U256>,
        _seen_competition_bid: Option<U256>,
    ) -> Result<crate::building::builders::block_building_helper::FinalizeBlockResult, crate::building::builders::block_building_helper::BlockBuildingHelperError> {
        unimplemented!("Mock implementation does not support finalize_block")
    }

    fn clone_cached_reads(&self) -> reth::revm::cached::CachedReads {
        reth::revm::cached::CachedReads::default()
    }

    fn built_block_trace(&self) -> &crate::building::BuiltBlockTrace {
        unimplemented!("Mock implementation does not support built_block_trace")
    }

    fn building_context(&self) -> &crate::building::BlockBuildingContext {
        unimplemented!("Mock implementation does not support building_context")
    }

    fn update_cached_reads(&mut self, _cached_reads: reth::revm::cached::CachedReads) {
    }

    fn builder_name(&self) -> &str {
        &self.builder_name
    }
}
use crate::utils::sgx_signature_verifier::BlockSignatureVerifier;

#[cfg(feature = "sgx_integration")]
use sgx_wasm_runner::BlockBuilderSgx;
use crate::live_builder::simulation::SimulatedOrderCommand;
use crate::provider;

#[derive(Debug)]
pub struct SgxWasmBlockBuildingAlgorithm {
    #[cfg(feature = "sgx_integration")]
    sgx_builder: BlockBuilderSgx,
    
    #[cfg(feature = "sgx_integration")]
    verifier: BlockSignatureVerifier,
    wasm_path: PathBuf,
    fallback_to_native: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedAccount {
    address: Address,
    balance: alloy_primitives::U256,
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
    bytecode: alloy_primitives::Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedTransaction {
    hash: B256,
    from: Address,
    to: Option<Address>,
    value: alloy_primitives::U256,
    gas_limit: u64,
    gas_price: Option<alloy_primitives::U256>,
    nonce: u64,
    input: alloy_primitives::Bytes,
    tx_type: String,
    access_list: Vec<AccessListItem>,
    blob_hashes: Vec<B256>,
    max_priority_fee_per_gas: Option<alloy_primitives::U256>,
    max_fee_per_gas: Option<alloy_primitives::U256>,
    max_fee_per_blob_gas: Option<alloy_primitives::U256>,
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
    transactions: Vec<alloy_primitives::Bytes>,
    receipts: Vec<SerializedReceipt>,
    state_diff: SerializedStateDiff,
    state_root: Option<B256>,
    metrics: BlockMetrics,
    signature: Option<alloy_primitives::Bytes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializedHeader {
    parent_hash: B256,
    number: u64,
    timestamp: u64,
    coinbase: Address,
    difficulty: alloy_primitives::U256,
    gas_limit: u64,
    gas_used: u64,
    base_fee_per_gas: alloy_primitives::U256,
    extra_data: alloy_primitives::Bytes,
    state_root: B256,
    transactions_root: B256,
    receipts_root: B256,
    #[serde(with = "serde_bytes_array")]
    logs_bloom: [u8; 256],
    mix_hash: B256,
    withdrawals_root: Option<B256>,
    blob_gas_used: Option<u64>,
    excess_blob_gas: Option<u64>,
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
    data: alloy_primitives::Bytes,
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
    old_balance: Option<alloy_primitives::U256>,
    new_balance: Option<alloy_primitives::U256>,
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
    bytecode: alloy_primitives::Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockMetrics {
    tx_count: usize,
    blob_count: usize,
    gas_used: u64,
    blob_gas_used: Option<u64>,
    block_value: alloy_primitives::U256,
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

impl SgxWasmBlockBuildingAlgorithm {
    pub fn new(wasm_path: PathBuf, fallback_to_native: bool) -> Result<Self> {
        #[cfg(not(feature = "sgx_integration"))]
        {
            warn!("SGX integration not enabled, SGX WASM builder will not be available");
            warn!("Build with --features sgx_integration to enable SGX support");
            
            return Ok(Self {
                wasm_path,
                fallback_to_native,
            });
        }
        
        #[cfg(feature = "sgx_integration")]
        {
            info!("Initializing SGX WASM block builder from {}", wasm_path.display());
            let sgx_builder = BlockBuilderSgx::new(&wasm_path)
                .map_err(|e| eyre!("Failed to initialize SGX WASM builder: {}", e))?;
            let public_key = sgx_builder.get_public_key()
                .map_err(|e| eyre!("Failed to get public key from SGX enclave: {}", e))?;
            
            info!("SGX WASM block builder initialized with public key: {}", public_key);
            let verifier = BlockSignatureVerifier::new(&public_key)
                .map_err(|e| eyre!("Failed to create signature verifier: {}", e))?;
            
            Ok(Self {
                sgx_builder,
                verifier,
                wasm_path,
                fallback_to_native,
            })
        }
    }
    async fn extract_state_data<P: provider::StateProviderFactory>(
        &self,
        provider_factory: &P,
        addresses: &[Address],
    ) -> Result<(Vec<SerializedAccount>, Vec<SerializedStorage>, Vec<SerializedCode>)>
    where
        P: provider::StateProviderFactory,
    {
        let mut accounts = Vec::new();
        let mut storage = Vec::new();
        let mut code = Vec::new();
        let mut code_hashes = std::collections::HashSet::new();

        let provider = provider_factory.latest()?;

        for &address in addresses {
            if let Some(account) = provider.basic_account(&address)? {
                accounts.push(SerializedAccount {
                    address,
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.bytecode_hash.unwrap_or_default(),
                });

                let empty_code_hash = B256::ZERO;
                if let Some(hash) = account.bytecode_hash {
                    if hash != empty_code_hash {
                        code_hashes.insert(hash);
                    }
                }

                let mut storage_keys = vec![
                    B256::ZERO,
                    B256::with_last_byte(1),
                    B256::with_last_byte(2),
                    B256::with_last_byte(3),
                ];

                if let Some(code_hash) = account.bytecode_hash {
                    if code_hash != B256::ZERO {
                        for i in 4..10 {
                            storage_keys.push(B256::with_last_byte(i));
                        }

                    }
                }

                storage_keys.sort();
                storage_keys.dedup();

                for &slot_key in &storage_keys {
                    if let Ok(Some(value)) = provider.storage(address, slot_key) {
                        storage.push(SerializedStorage {
                            address,
                            slot: slot_key,
                            value: value.into(),
                        });
                    }
                }
            }
        }

        for code_hash in code_hashes {
            if let Some(bytecode) = provider.bytecode_by_hash(&code_hash)? {
                code.push(SerializedCode {
                    hash: code_hash,
                    bytecode: bytecode.bytecode().clone(),
                });
            }
        }

        Ok((accounts, storage, code))
    }
    fn convert_orders(
        &self,
        orders: &ValidatedOrderMap,
    ) -> (Vec<SerializedTransaction>, Vec<SerializedBundle>) {
        let mut transactions = Vec::new();
        let mut bundles = Vec::new();

        let validated_orders = orders.get_validated_orders();
        for (order_id, order) in validated_orders {
            match order {
                crate::primitives::Order::Tx(tx) => {
                    let tx = &tx.tx_with_blobs;

                    let inner_tx = tx.internal_tx_unsecure().transaction();
                    let from = tx.signer();
                    let hash = tx.hash();

                    let serialized_tx = self.serialize_transaction(inner_tx, from, hash);
                    transactions.push(serialized_tx);
                },

                crate::primitives::Order::Bundle(bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();

                        let serialized_tx = self.serialize_transaction(inner_tx, from, hash);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                            id: order_id,
                            transactions: bundle_transactions,
                        });
                    }
                },

                crate::primitives::Order::ShareBundle(share_bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in share_bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();

                        let serialized_tx = self.serialize_transaction(inner_tx, from, hash);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                            id: order_id,
                            transactions: bundle_transactions,
                        });
                    }
                }
            }
        }

        (transactions, bundles)
    }
    fn serialize_transaction(&self, tx: &reth_primitives::Transaction, sender: Address, hash: B256) -> SerializedTransaction {
        let tx_type = match tx.tx_type() {
            alloy_consensus::TxType::Legacy => "Legacy",
            alloy_consensus::TxType::Eip2930 => "AccessList",
            alloy_consensus::TxType::Eip1559 => "EIP1559",
            alloy_consensus::TxType::Eip4844 => "EIP4844",
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
    fn convert_block_params(&self, ctx: &BlockBuildingContext) -> BlockParams {
        BlockParams {
            number: ctx.evm_env.block_env.number.to::<u64>(),
            timestamp: ctx.attributes.timestamp,
            gas_limit: ctx.evm_env.block_env.gas_limit.to::<u64>(),
            base_fee_per_gas: format!("0x{:x}", ctx.evm_env.block_env.basefee),
            coinbase: ctx.attributes.suggested_fee_recipient,
            parent_hash: ctx.attributes.parent,
            parent_state_root: B256::default(),
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
        }
    }
    fn create_config(&self, ctx: &BlockBuildingContext) -> BlockBuilderConfig {
        BlockBuilderConfig {
            discard_txs: true,
            sorting: "MevGasPrice".to_string(),
            failed_tx_retries: 1,
            drop_failed_txs: true,
            coinbase_payment: !ctx.coinbase_is_suggested_fee_recipient(),
            build_timeout_ms: 5000,
        }
    }
    fn convert_output_to_block(&self, output: BlockBuilderOutput, _helper: &dyn BlockBuildingHelper, _ctx: &BlockBuildingContext) -> Result<BiddableUnfinishedBlock> {
        let block_value = output.metrics.block_value;

        info!("SGX WASM Builder produced block with value: {}", block_value);

        let mock_helper = MockBlockBuildingHelper::new(
            "SGX WASM Builder".to_string(),
            block_value,
        );

        let unfinished_block = block_building_helper::BiddableUnfinishedBlock::new(
            Box::new(mock_helper)
        )?;

        Ok(unfinished_block)
    }
}

impl<P> BlockBuildingAlgorithm<P> for SgxWasmBlockBuildingAlgorithm
where
    P: provider::StateProviderFactory + Send + Sync + 'static + Clone,
{
    fn name(&self) -> String {
        "SGX WASM Builder".to_string()
    }

    fn build_blocks(&self, input: BlockBuildingAlgorithmInput<P>) {
        let name: String = <Self as BlockBuildingAlgorithm<P>>::name(self);

        let wasm_path = self.wasm_path.clone();
        let fallback_to_native = self.fallback_to_native;

        #[cfg(feature = "sgx_integration")]
        let (sgx_builder, verifier) = {
            let sgx_builder = self.sgx_builder.clone();
            let verifier = self.verifier.clone();
            (sgx_builder, verifier)
        };

        let ctx = input.ctx.clone();
        let sink = input.sink.clone();
        let cancel = input.cancel.clone();
        let provider = input.provider.clone();

        let (order_sender, _) = tokio::sync::broadcast::channel::<SimulatedOrderCommand>(1024);
        let mut input_receiver = input.input;

        let order_sender_clone = order_sender.clone();

        #[cfg(feature = "sgx_integration")]
        let owned_self = SgxWasmBlockBuildingAlgorithm {
            sgx_builder,
            verifier,
            wasm_path: wasm_path.clone(),
            fallback_to_native,
        };

        #[cfg(not(feature = "sgx_integration"))]
        let owned_self = SgxWasmBlockBuildingAlgorithm {
            wasm_path: wasm_path.clone(),
            fallback_to_native,
        };

        tokio::spawn({
            let cancel = cancel.clone();
            async move {
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }

                    match input_receiver.recv().await {
                        Ok(cmd) => {
                            let _ = order_sender.send(cmd);
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Message forwarding lagged by {} messages", n);
                        }
                    }
                }
            }
        });

        tokio::spawn(async move {
            let mut order_batch_count = 0;

            while !cancel.is_cancelled() {
                let ordering_id = OrderingId(format!("sgx-wasm-{}-{}", uuid::Uuid::new_v4(), order_batch_count));
                order_batch_count += 1;
                let state_provider = match provider.latest() {
                    Ok(provider) => Arc::from(provider) as Arc<dyn StateProvider>,
                    Err(e) => {
                        error!("Failed to get latest state provider: {}", e);
                        continue;
                    }
                };
                let helper = match block_building_helper::BlockBuildingHelperFromProvider::new(
                    state_provider.clone(),
                    ctx.clone(),
                    None,
                    name.clone(),
                    true,
                    cancel.clone(), 
                ) {
                    Ok(helper) => helper,
                    Err(e) => {
                        error!("Failed to create block building helper: {}", e);
                        continue;
                    }
                };

                let mut order_intake_consumer =
                    OrderIntakeConsumer::<self::ValidOrderPriority>::new(
                        NonceCache::new(state_provider),
                        order_sender_clone.subscribe()
                    );

                let keep_running = match order_intake_consumer.blocking_consume_next_batch() {
                    Ok(true) => true,
                    Ok(false) => {
                        debug!("Order consumer channel closed, stopping SGX WASM builder");
                        break;
                    },
                    Err(e) => {
                        error!("Failed to consume next batch of orders: {}", e);
                        continue;
                    }
                };

                if !keep_running {
                    continue;
                }

                let block_orders = order_intake_consumer.current_block_orders();
                if block_orders.is_empty() {
                    debug!("No validated orders to process");
                    continue;
                }
                let (tx, rx) = oneshot::channel::<BiddableUnfinishedBlock>();
                
                #[cfg(feature = "sgx_integration")]
                {
                    info!("Building block using SGX enclave");

                    let mut addresses = Vec::new();
                    let mut access_list_entries: std::collections::HashMap<Address, std::collections::HashSet<B256>> =
                        std::collections::HashMap::new();

                    for order in block_orders.get_all_orders() {
                        match &order.order {
                            Order::Tx(tx) => {
                                addresses.push(tx.tx_with_blobs.signer());

                                if let Some(to) = tx.tx_with_blobs.to() {
                                    addresses.push(to);
                                }

                                let inner_tx = tx.tx_with_blobs.internal_tx_unsecure().transaction();
                                if let Some(access_list) = inner_tx.access_list() {
                                    for item in &access_list.0 {
                                        addresses.push(item.address);

                                        access_list_entries
                                            .entry(item.address)
                                            .or_insert_with(std::collections::HashSet::new)
                                            .extend(item.storage_keys.iter().cloned());
                                    }
                                }
                            },
                            Order::Bundle(bundle) => {
                                for (tx, _) in bundle.list_txs() {
                                    addresses.push(tx.signer());

                                    if let Some(to) = tx.to() {
                                        addresses.push(to);
                                    }

                                    let inner_tx = tx.internal_tx_unsecure().transaction();
                                    if let Some(access_list) = inner_tx.access_list() {
                                        for item in &access_list.0 {
                                            addresses.push(item.address);

                                            access_list_entries
                                                .entry(item.address)
                                                .or_insert_with(std::collections::HashSet::new)
                                                .extend(item.storage_keys.iter().cloned());
                                        }
                                    }
                                }
                            },
                            Order::ShareBundle(share_bundle) => {
                                for (tx, _) in share_bundle.list_txs() {
                                    addresses.push(tx.signer());

                                    if let Some(to) = tx.to() {
                                        addresses.push(to);
                                    }

                                    let inner_tx = tx.internal_tx_unsecure().transaction();
                                    if let Some(access_list) = inner_tx.access_list() {
                                        for item in &access_list.0 {
                                            addresses.push(item.address);

                                            access_list_entries
                                                .entry(item.address)
                                                .or_insert_with(std::collections::HashSet::new)
                                                .extend(item.storage_keys.iter().cloned());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !access_list_entries.is_empty() {
                        info!(
                            "Collected access list entries for {} addresses with a total of {} storage keys",
                            access_list_entries.len(),
                            access_list_entries.values().map(|keys| keys.len()).sum::<usize>()
                        );
                    }

                    info!("Extracting state data for {} addresses", addresses.len());

                    if !access_list_entries.is_empty() {
                        info!("Including {} storage keys from transaction access lists",
                            access_list_entries.values().map(|keys| keys.len()).sum::<usize>());
                    }

                    addresses.sort();
                    addresses.dedup();

                    let (accounts, mut storage, code) = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            owned_self.extract_state_data(&provider, &addresses).await
                        })
                    }).unwrap_or_else(|e| {
                        error!("Failed to extract state data: {}", e);
                        (Vec::new(), Vec::new(), Vec::new())
                    });

                    info!("Extracted {} accounts, {} storage slots, and {} code entries",
                        accounts.len(), storage.len(), code.len());

                    let mut additional_storage = Vec::new();
                    for (address, keys) in &access_list_entries {
                        for &key in keys {
                            if let Ok(Some(value)) = provider.latest()
                                .expect("[ERROR] can't reach this")
                                .storage(*address, key) {
                                additional_storage.push(SerializedStorage {
                                    address: *address,
                                    slot: key,
                                    value: value.into(),
                                });
                            }
                        }
                    }

                    if !additional_storage.is_empty() {
                        info!("Adding {} additional storage slots from access lists", additional_storage.len());
                        storage.extend(additional_storage);
                    }

                    let mut serialized_data = BlockBuilderInput {
                        block_params: owned_self.convert_block_params(&ctx),
                        accounts,
                        storage,
                        code,
                        transactions: Vec::new(),
                        bundles: Vec::new(),
                        config: owned_self.create_config(&ctx),
                    };

                    let (transactions, bundles) = owned_self.convert_orders(&block_orders);
                    serialized_data.transactions = transactions;
                    serialized_data.bundles = bundles;

                    let orders_json = match serde_json::to_string(&serialized_data) {
                        Ok(json) => json,
                        Err(e) => {
                            error!("Failed to serialize orders for SGX: {}", e);
                            continue;
                        }
                    };
                    match owned_self.sgx_builder.build_block(&orders_json) {
                        Ok(output_json) => {
                            match owned_self.verifier.verify(&output_json) {
                                Ok(_) => {
                                    info!("Successfully verified SGX enclave signature");
                                    match serde_json::from_str::<BlockBuilderOutput>(&output_json) {
                                        Ok(output) => {
                                            match owned_self.convert_output_to_block(output, &helper, &ctx) {
                                                Ok(block) => {
                                                    let _ = tx.send(block);
                                                },
                                                Err(e) => {
                                                    error!("Failed to convert SGX output to block: {}", e);
                                                    continue;
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            error!("Failed to parse SGX output: {}", e);
                                            continue;
                                        }
                                    }
                                },
                                Err(e) => {
                                    error!("SGX signature verification failed: {}", e);
                                    continue;
                                }
                            }
                        },
                        Err(e) => {
                            error!("Failed to build block with SGX: {}", e);
                            
                            if fallback_to_native {
                                warn!("Falling back to native block building");
                                let fallback_helper = MockBlockBuildingHelper::new(
                                    format!("{}-fallback", name),
                                    U256::from(0),
                                );
                                let fallback_block = match block_building_helper::BiddableUnfinishedBlock::new(Box::new(fallback_helper)) {
                                    Ok(block) => block,
                                    Err(e) => {
                                        error!("Failed to create fallback BiddableUnfinishedBlock: {}", e);
                                        continue;
                                    }
                                };
                                let _ = tx.send(fallback_block);
                            } else {
                                continue;
                            }
                        }
                    }
                }
                
                #[cfg(not(feature = "sgx_integration"))]
                {
                    error!("SGX integration not enabled, cannot build blocks");
                    let dummy_helper = MockBlockBuildingHelper::new(
                        name.clone(),
                        U256::from(0),
                    );
                    let dummy_block = match block_building_helper::BiddableUnfinishedBlock::new(Box::new(dummy_helper)) {
                        Ok(block) => block,
                        Err(e) => {
                            error!("Failed to create BiddableUnfinishedBlock: {}", e);
                            continue;
                        }
                    };
                    let _ = tx.send(dummy_block);
                }
                if let Ok(block) = rx.await {
                    let block_value = block.true_block_value();
                    info!("Built block with value: {}", block_value);

                    sink.new_block(block);
                }
            }
        });
    }
}