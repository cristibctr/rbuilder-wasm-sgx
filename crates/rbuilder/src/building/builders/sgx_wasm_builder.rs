use std::fmt::Debug;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::collections::HashMap;

use alloy_primitives::{Address, Bytes, StorageValue, B256, U256 as AlloyU256, keccak256, FixedBytes};
use alloy_rlp::{Encodable as AlloyEncodable, Decodable as AlloyDecodable, RlpEncodable, RlpDecodable};
use eyre::{eyre, Result, Report as ErrReport};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error, debug, trace};
use tokio::sync::oneshot;
use std::hash::Hash;
use alloy_consensus::{Transaction, TxType, EMPTY_ROOT_HASH};
use alloy_eips::merge::BEACON_NONCE;
use alloy_eips::eip4895::Withdrawal;
use reth_node_api::{FullNodeComponents, FullNodeTypes, PayloadBuilderAttributes};
use reth_provider::{AccountReader, StateProvider, StateProviderFactory};
use reth_errors::ProviderError;
use reth_transaction_pool::PoolTransaction;
use reth_primitives_traits::{SignedTransaction, Account};
use reth_primitives::{
    TransactionSigned, Header as RethHeader, Receipt as RethReceipt, Log as RethLog, LogData,
};
use alloy_primitives::Bloom;
use crate::building::{Address as RethAddress, Bytes as RethBytes, B256 as RethB256, U256 as RethU256};
use crate::building::proofs;
const EMPTY_CODE_HASH: B256 = KECCAK_EMPTY;
const EMPTY_TRIE_ROOT: B256 = EMPTY_ROOT_HASH;
use reth_chainspec::{ChainSpec, EthereumHardforks};
use eth_sparse_mpt::sparse_mpt::{
    diff_trie::{DiffTrie, DiffTrieNode, DiffTrieNodeKind},
};
use reth_trie::Nibbles;
const KECCAK_EMPTY_TRIE_HASH: B256 = EMPTY_ROOT_HASH;


use crate::building::{
    builders::{
        BlockBuildingAlgorithm, BlockBuildingAlgorithmInput, BlockBuildingContext,
        block_building_helper::{self, BlockBuildingHelper, BiddableUnfinishedBlock, BlockBuildingHelperError, FinalizeBlockResult},
        OrderIntakeConsumer, Block,
    },
    BuiltBlockTrace, CriticalCommitOrderError, ExecutionError, ExecutionResult, PrioritizedOrderStore, OrderPriority,
    SimulatedOrderSink, simulated_order_command_to_sink,
};
use crate::telemetry;

fn add_histogram_value_in_tests(name: &str, value: f64) {
    crate::telemetry::TXFETCHER_TRANSACTION_QUERY_TIME
        .with_label_values(&[])
        .observe(value);
    debug!("METRICS: add_histogram_value {} = {}", name, value);
}

fn add_counter_value_in_tests(name: &str, value: u64) {
    for _ in 0..value {
        crate::telemetry::TXFETCHER_TRANSACTION_COUNTER.inc();
    }
    debug!("METRICS: add_counter_value {} = {}", name, value);
}
use crate::utils::NonceCache;
use crate::primitives::SimValue;
use std::sync::Arc;
use std::cmp::Ordering;
use alloy_consensus::constants::KECCAK_EMPTY;

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

use time::OffsetDateTime;
use uuid::Uuid;
use alloy_primitives::U256;
use alloy_provider::Provider;
use alloy_rlp::Encodable;
use reth::providers::BlockNumReader;

#[derive(Debug, Clone)]
struct OrderingId(String);

use crate::primitives::Order;

struct SgxWasmBlockBuildingHelper {
    block_value: U256,
    builder_name: String,
    block_trace: BuiltBlockTrace,
    block_context: BlockBuildingContext,
    cached_reads: reth::revm::cached::CachedReads,
    coinbase_profit: AlloyU256,
    executed_txs: Vec<crate::primitives::TransactionSignedEcRecoveredWithBlobs>,
    receipts: Vec<RethReceipt>,
    blob_sidecars: Vec<Arc<alloy_eips::eip4844::BlobTransactionSidecar>>,
    execution_requests: Vec<alloy_primitives::Bytes>,
    sgx_output_header: SerializedHeader,
    sgx_state_diff: SerializedStateDiff,
}

impl SgxWasmBlockBuildingHelper {
    pub fn new(
        builder_name: String,
        block_value: AlloyU256,
        block_context: BlockBuildingContext,
        executed_txs: Vec<crate::primitives::TransactionSignedEcRecoveredWithBlobs>,
        receipts: Vec<RethReceipt>,
        blob_sidecars: Vec<Arc<alloy_eips::eip4844::BlobTransactionSidecar>>,
        execution_requests: Vec<alloy_primitives::Bytes>,
        sgx_output_header: SerializedHeader,
        sgx_state_diff: SerializedStateDiff,
    ) -> Self {
        Self {
            block_value,
            builder_name,
            block_trace: BuiltBlockTrace::new(),
            block_context,
            cached_reads: reth::revm::cached::CachedReads::default(),
            coinbase_profit: block_value,
            executed_txs,
            receipts,
            blob_sidecars,
            execution_requests,
            sgx_output_header,
            sgx_state_diff,
        }
    }
}

impl block_building_helper::BlockBuildingHelper for SgxWasmBlockBuildingHelper {
    fn box_clone(&self) -> Box<dyn block_building_helper::BlockBuildingHelper> {
        Box::new(Self {
            block_value: self.block_value,
            builder_name: self.builder_name.clone(),
            block_trace: self.block_trace.clone(),
            block_context: self.block_context.clone(),
            cached_reads: self.cached_reads.clone(),
            coinbase_profit: self.coinbase_profit,
            executed_txs: self.executed_txs.clone(),
            receipts: self.receipts.clone(),
            blob_sidecars: self.blob_sidecars.clone(),
            execution_requests: self.execution_requests.clone(),
            sgx_output_header: self.sgx_output_header.clone(),
            sgx_state_diff: self.sgx_state_diff.clone(),
        })
    }

    fn commit_order(
        &mut self,
        _order: &crate::primitives::SimulatedOrder,
        _result_filter: &dyn Fn(&crate::primitives::SimValue) -> Result<(), crate::building::ExecutionError>,
    ) -> Result<Result<&crate::building::ExecutionResult, crate::building::ExecutionError>, crate::building::CriticalCommitOrderError> {
        let error_msg = std::io::Error::new(
            std::io::ErrorKind::Unsupported, 
            "SGX WASM builder doesn't support incremental order building"
        );
        let err = ProviderError::other(error_msg);
        Err(CriticalCommitOrderError::Reth(err))
    }

    fn set_trace_fill_time(&mut self, time: std::time::Duration) {
        self.block_trace.fill_time = time;
    }

    fn set_trace_orders_closed_at(&mut self, orders_closed_at: time::OffsetDateTime) {
        self.block_trace.orders_closed_at = orders_closed_at;
    }

    fn can_add_payout_tx(&self) -> bool {
        true
    }

    fn true_block_value(&self) -> std::result::Result<AlloyU256, BlockBuildingHelperError> {
        Ok(self.block_value)
    }

    fn finalize_block(
        mut self: Box<Self>,
        _payout_tx_value: Option<AlloyU256>,
        seen_competition_bid: Option<AlloyU256>,
    ) -> Result<crate::building::builders::block_building_helper::FinalizeBlockResult, crate::building::builders::block_building_helper::BlockBuildingHelperError> {
        let start_time = Instant::now();
        
        self.block_trace.seen_competition_bid = seen_competition_bid;
        self.block_trace.update_orders_sealed_at();
        
        let ctx = &self.block_context;
        
        let gas_used = self.receipts.iter().map(|r| r.cumulative_gas_used).sum::<u64>();
        
        let blob_gas_used = if ctx.chain_spec.is_cancun_active_at_timestamp(ctx.attributes.timestamp) {
            let blob_count = self.blob_sidecars.len();
            if blob_count > 0 {
                let blob_gas_per_blob = 131072;
                Some(blob_count as u64 * blob_gas_per_blob)
            } else {
                Some(0)
            }
        } else {
            None
        };
        
        let logs_bloom = if !self.receipts.is_empty() {
            Bloom::from_slice(&self.sgx_output_header.logs_bloom)
        } else {
            Bloom::default()
        };
        
        let withdrawals_root = if ctx.chain_spec.is_shanghai_active_at_timestamp(ctx.attributes.timestamp) {
            let withdrawals: &[Withdrawal] = &[];
            Some(proofs::calculate_withdrawals_root(withdrawals))
        } else {
            None
        };
        
        let transactions_for_root: Vec<TransactionSigned> = self.executed_txs.iter()
            .map(|tx| tx.internal_tx_unsecure().tx().clone())
            .collect();
        let transactions_root = proofs::calculate_transaction_root(&transactions_for_root);
        
        let receipts_root = {
            if self.receipts.is_empty() {
                EMPTY_ROOT_HASH
            } else {
                self.sgx_output_header.receipts_root
            }
        };
        
        let root_hasher_start = Instant::now();
        let state_root = {
            let mut state_trie = DiffTrie::new_empty();
            let mut account_storage_tries: HashMap<Address, DiffTrie> = HashMap::new();

            let mut storage_diff_by_account: HashMap<Address, Vec<&SerializedStorageDiff>> = HashMap::new();
            for storage_item in &self.sgx_state_diff.storage {
                storage_diff_by_account.entry(storage_item.address).or_default().push(storage_item);
            }

            for acc_diff in &self.sgx_state_diff.accounts {
                let address = acc_diff.address;
                let hashed_address = keccak256(address.as_slice());

                let storage_root = if let Some(storage_diffs) = storage_diff_by_account.get(&address) {
                    let storage_trie = account_storage_tries.entry(address).or_insert_with(DiffTrie::new_empty);
                    for s_diff in storage_diffs {
                        let slot_hash = keccak256(s_diff.slot.as_slice());
                        let nibbles = Nibbles::unpack(&slot_hash);
                        
                        let mut rlp_value = Vec::new();
                        AlloyEncodable::encode(&AlloyU256::from_be_bytes(s_diff.new_value.0), &mut rlp_value);

                        storage_trie.insert(nibbles.to_vec().into(), rlp_value.into())
                            .map_err(|e| BlockBuildingHelperError::from(eyre!("Failed to insert into storage trie: {:?}", e)))?;
                    }
                    storage_trie.root_hash().map_err(|e| BlockBuildingHelperError::from(eyre!("Failed to compute storage trie hash: {:?}", e)))?
                } else {
                    acc_diff.old_code_hash.and_then(|och| if och == EMPTY_CODE_HASH { Some(EMPTY_TRIE_ROOT) } else { None} )
                        .unwrap_or(KECCAK_EMPTY_TRIE_HASH)
                };

                let nonce = acc_diff.new_nonce.unwrap_or_default();
                let balance = acc_diff.new_balance.unwrap_or_default();
                let code_hash = acc_diff.new_code_hash.or(acc_diff.old_code_hash).filter(|&h| h != KECCAK_EMPTY)
                    .unwrap_or(KECCAK_EMPTY);
                
                let mut rlp_account_data: Vec<u8> = Vec::new();
                
                alloy_rlp::Encodable::encode(&nonce, &mut rlp_account_data);
                alloy_rlp::Encodable::encode(&balance, &mut rlp_account_data);
                alloy_rlp::Encodable::encode(&storage_root, &mut rlp_account_data);
                alloy_rlp::Encodable::encode(&code_hash, &mut rlp_account_data);
                
                let nibbles = Nibbles::unpack(&hashed_address);
                state_trie.insert(nibbles.to_vec().into(), rlp_account_data.into())
                    .map_err(|e| BlockBuildingHelperError::from(eyre!("Failed to insert into state trie: {:?}", e)))?;
            }
             state_trie.root_hash().map_err(|e| BlockBuildingHelperError::from(eyre!("Failed to compute state trie hash: {:?}", e)))?
        };
        
        let root_hash_time = root_hasher_start.elapsed();
        self.block_trace.root_hash_time = root_hash_time;

        
        let block_number = ctx.evm_env.block_env.number.to::<u64>();
        
        let header = RethHeader {
            parent_hash: ctx.attributes.parent,
            ommers_hash: EMPTY_ROOT_HASH,
            beneficiary: ctx.evm_env.block_env.coinbase,
            state_root,
            transactions_root,
            receipts_root,
            logs_bloom,
            difficulty: AlloyU256::ZERO.into(),
            number: block_number,
            gas_limit: ctx.evm_env.block_env.gas_limit.to::<u64>(),
            gas_used,
            timestamp: ctx.attributes.timestamp,
            extra_data: ctx.extra_data.clone().into(),
            mix_hash: ctx.attributes.prev_randao,
            nonce: FixedBytes::<8>::from(BEACON_NONCE),
            base_fee_per_gas: Some(ctx.evm_env.block_env.basefee.to()),
            withdrawals_root,
            blob_gas_used,
            excess_blob_gas: ctx.excess_blob_gas.map(|val| val.into()),
            parent_beacon_block_root: ctx.attributes.parent_beacon_block_root,
            requests_hash: None,
        };
        
        let reth_transactions: Vec<TransactionSigned> = self.executed_txs.iter()
            .map(|tx| tx.internal_tx_unsecure().tx().clone())
            .collect();

        let body = reth::primitives::BlockBody {
            transactions: reth_transactions,
            ommers: vec![],
            withdrawals: None,
        };
        
        let block_with_senders = reth::primitives::Block { header: header.clone(), body };
        
        self.block_trace.finalize_time = start_time.elapsed();
        
        info!(
            "SGX Block finalized: block={}, gas_used={}, tx_count={}, blob_count={}, value={}, finalize_time={:?}",
            block_number,
            gas_used,
            self.executed_txs.len(),
            self.blob_sidecars.len(),
            self.block_value,
            self.block_trace.finalize_time
        );
        
        Ok(FinalizeBlockResult {
            block: crate::building::builders::Block {
                trace: self.block_trace.clone(),
                sealed_block: block_with_senders.into(),
                txs_blobs_sidecars: self.blob_sidecars.clone(),
                execution_requests: self.execution_requests.clone(),
                builder_name: self.builder_name.clone(),
            },
            cached_reads: self.cached_reads.clone(),
        })
    }

    fn clone_cached_reads(&self) -> reth::revm::cached::CachedReads {
        self.cached_reads.clone()
    }

    fn built_block_trace(&self) -> &crate::building::BuiltBlockTrace {
        &self.block_trace
    }

    fn building_context(&self) -> &crate::building::BlockBuildingContext {
        &self.block_context
    }

    fn update_cached_reads(&mut self, cached_reads: reth::revm::cached::CachedReads) {
        self.cached_reads = cached_reads;
    }

    fn builder_name(&self) -> &str {
        &self.builder_name
    }
}
use crate::utils::sgx_signature_verifier::BlockSignatureVerifier;
use alloy_primitives::Sealable;
use futures::TryFutureExt;
#[cfg(feature = "sgx_integration")]
use sgx_wasm_runner::BlockBuilderSgx;
use crate::live_builder::simulation::SimulatedOrderCommand;
use crate::provider;

impl From<eyre::Report> for BlockBuildingHelperError {
    fn from(err: eyre::Report) -> Self {
        let error_str = err.to_string();
        let std_error = std::io::Error::new(std::io::ErrorKind::Other, error_str);
        BlockBuildingHelperError::ProviderError(reth_errors::ProviderError::other(std_error))
    }
}

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
            warn!("[SGX DEBUG] SGX integration not enabled, SGX WASM builder will not be available");
            warn!("[SGX DEBUG] Build with --features sgx_integration to enable SGX support");
            
            return Ok(Self {
                wasm_path,
                fallback_to_native,
            });
        }
        
        #[cfg(feature = "sgx_integration")]
        {
            info!("[SGX DEBUG] Initializing SGX WASM block builder from {}", wasm_path.display());
            
            if !wasm_path.exists() {
                error!("[SGX DEBUG] WASM file not found at: {}", wasm_path.display());
                return Err(eyre!("WASM file not found at: {}", wasm_path.display()));
            }
            
            let parent_dir = wasm_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            info!("[SGX DEBUG] Contents of directory: {}", parent_dir.display());
            if let Ok(entries) = std::fs::read_dir(parent_dir) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        info!("[SGX DEBUG] - {}", entry.path().display());
                    }
                }
            }
            
            if let Ok(metadata) = std::fs::metadata(&wasm_path) {
                info!("[SGX DEBUG] WASM file size: {} bytes", metadata.len());
            }
            
            info!("[SGX DEBUG] Creating BlockBuilderSgx instance");
            let sgx_builder = match BlockBuilderSgx::new(&wasm_path) {
                Ok(builder) => {
                    info!("[SGX DEBUG] Successfully created BlockBuilderSgx instance");
                    builder
                },
                Err(e) => {
                    error!("[SGX DEBUG] Failed to initialize SGX WASM builder: {}", e);
                    return Err(eyre!("Failed to initialize SGX WASM builder: {}", e));
                }
            };
            
            info!("[SGX DEBUG] Getting public key from SGX enclave");
            let public_key = match sgx_builder.get_public_key() {
                Ok(key) => {
                    info!("[SGX DEBUG] Successfully retrieved public key from SGX enclave");
                    key
                },
                Err(e) => {
                    error!("[SGX DEBUG] Failed to get public key from SGX enclave: {}", e);
                    return Err(eyre!("Failed to get public key from SGX enclave: {}", e));
                }
            };
            
            info!("[SGX DEBUG] SGX WASM block builder initialized with public key: {}", public_key);
            
            info!("[SGX DEBUG] Creating signature verifier");
            let verifier = match BlockSignatureVerifier::new(&public_key) {
                Ok(v) => {
                    info!("[SGX DEBUG] Successfully created signature verifier");
                    v
                },
                Err(e) => {
                    error!("[SGX DEBUG] Failed to create signature verifier: {}", e);
                    return Err(eyre!("Failed to create signature verifier: {}", e));
                }
            };
            
            info!("[SGX DEBUG] SGX WASM block builder fully initialized");
            
            Ok(Self {
                sgx_builder,
                verifier,
                wasm_path,
                fallback_to_native,
            })
        }
    }
    
    type CacheKey = (u64, Vec<Address>);
    
    thread_local! {
        static STATE_CACHE: std::cell::RefCell<
            std::collections::HashMap<SgxWasmBlockBuildingAlgorithm::CacheKey, 
                (Vec<SerializedAccount>, Vec<SerializedStorage>, Vec<SerializedCode>)>
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    
    fn extract_state_data<P: provider::StateProviderFactory>(
        &self,
        provider_factory: &P,
        block_number: u64,
        addresses: &[Address],
    ) -> Result<(Vec<SerializedAccount>, Vec<SerializedStorage>, Vec<SerializedCode>)>
    where
        P: provider::StateProviderFactory,
    {
        let provider = provider_factory.latest()?;
        
        let mut sorted_addresses = addresses.to_vec();
        sorted_addresses.sort();
        sorted_addresses.dedup();
        
        let cache_key = (block_number, sorted_addresses.clone());
        
        let cache_hit = Self::STATE_CACHE.with(|cache| {
            let cache = cache.borrow();
            cache.get(&cache_key).cloned()
        });
        
        if let Some(cached_data) = cache_hit {
            debug!("[SGX DEBUG] Cache hit for state data at block {}", block_number);
            return Ok(cached_data);
        }
        
        debug!("[SGX DEBUG] Cache miss for state data at block {}", block_number);
        
        let mut accounts = Vec::new();
        let mut storage = Vec::new();
        let mut code = Vec::new();
        let mut code_hashes = std::collections::HashSet::new();

        for &address in &sorted_addresses {
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

        let result = (accounts.clone(), storage.clone(), code.clone());
        Self::STATE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() > 10 {
                let keys: Vec<_> = cache.keys().cloned().collect();
                for key in keys.iter().take(keys.len() - 10) {
                    cache.remove(key);
                }
            }
            cache.insert(cache_key, result.clone());
        });

        Ok(result)
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
            blob_hashes: Vec::new(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas().map(|fee| U256::from(fee)),
            max_fee_per_gas: Some(U256::from(tx.max_fee_per_gas())),
            max_fee_per_blob_gas: tx.max_fee_per_blob_gas().map(|fee| U256::from(fee)),
            versioned_hashes: Vec::new(),
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
    fn convert_output_to_block(&self, output: BlockBuilderOutput, helper: &dyn BlockBuildingHelper, ctx: &BlockBuildingContext) -> Result<BiddableUnfinishedBlock> {
        let block_value = output.metrics.block_value;

        info!("SGX WASM Builder produced block with value: {}", block_value);
        
        let mut executed_txs = Vec::new();
        let mut receipts_vec = Vec::new();
        let mut blob_sidecars = Vec::new();
        
        for tx_bytes_alloy in &output.transactions {
            let mut tx_bytes_slice: &[u8] = &tx_bytes_alloy.0;
            if let Ok(tx_signed_reth) = TransactionSigned::decode(&mut tx_bytes_slice) {
                let recover_result = tx_signed_reth.recover_signer();
                if let Ok(signer) = recover_result {
                    let recovered = reth_primitives::Recovered::new_unchecked(tx_signed_reth, signer);
                    let tx_with_blobs = crate::primitives::TransactionSignedEcRecoveredWithBlobs::new_no_blobs(recovered)
                        .map_err(|e| eyre!("Failed to create TransactionSignedEcRecoveredWithBlobs: {}", e))?;
                    executed_txs.push(tx_with_blobs);
                } else {
                    warn!("Failed to recover signer for transaction from SGX output");
                }
            } else {
                warn!("Failed to decode transaction from SGX output: {:?}", tx_bytes_alloy);
            }
        }
        
        for receipt_data in &output.receipts {
            let mut logs = Vec::new();
            for log_data in &receipt_data.logs {
                let reth_log_data = LogData::new_unchecked(
                    log_data.topics.iter().map(|t| RethB256::from_slice(t.as_slice())).collect(), 
                    RethBytes(log_data.data.0.clone())
                );
                logs.push(RethLog {
                    address: RethAddress::from_slice(log_data.address.as_slice()),
                    data: reth_log_data,
                });
            }
            
            let reth_tx_type = match receipt_data.tx_type {
                0 => reth_primitives::TxType::Legacy,
                1 => reth_primitives::TxType::Eip2930,
                2 => reth_primitives::TxType::Eip1559,
                3 => reth_primitives::TxType::Eip4844,
                _ => {
                    warn!("Unknown transaction type {} in SGX receipt, defaulting to Legacy", receipt_data.tx_type);
                    reth_primitives::TxType::Legacy
                }
            };
            
            receipts_vec.push(RethReceipt {
                tx_type: reth_tx_type,
                success: receipt_data.success,
                cumulative_gas_used: receipt_data.cumulative_gas_used,
                logs,
            });
        }
        
        let sgx_state_diff = output.state_diff.clone();
        
        let execution_requests = Vec::new();
        
        let mut block_trace = BuiltBlockTrace::new();
        if let Some(trace_info) = &output.metrics.trace {
            block_trace.fill_time = Duration::from_micros(trace_info.sim_time_us);
        }
        
        block_trace.bid_value = output.metrics.block_value;
        block_trace.coinbase_reward = output.metrics.block_value;
        block_trace.true_bid_value = output.metrics.block_value;
        
        let sgx_helper = SgxWasmBlockBuildingHelper::new(
            "SGX WASM Builder".to_string(),
            block_value,
            ctx.clone(),
            executed_txs,
            receipts_vec,
            blob_sidecars,
            execution_requests,
            output.header.clone(),
            sgx_state_diff,
        );
        
        let unfinished_block = block_building_helper::BiddableUnfinishedBlock::new(
            Box::new(sgx_helper)
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
        
        let join_set = std::sync::Arc::new(tokio::sync::Mutex::new(
            tokio::task::JoinSet::new()
        ));

        let (order_sender, order_receiver) = tokio::sync::broadcast::channel::<SimulatedOrderCommand>(1024);
        let mut input_receiver = input.input;

        let mut owned_self = SgxWasmBlockBuildingAlgorithm {
            sgx_builder,
            verifier,
            wasm_path: wasm_path.clone(),
            fallback_to_native,
        };
        let cancel_token = input.cancel.clone();
        {
            let cancel = cancel_token.clone();
            let join_set_clone = join_set.clone();
            tokio::spawn(async move {
                let mut join_set = join_set_clone.lock().await;
                join_set.spawn(async move {
                    let cancel = cancel.clone();
                    let order_receiver = order_receiver;
                    
                    info!("[SGX DEBUG] Starting SGX WASM Builder worker loop with dedicated receiver");
                    
                    let mut order_batch_count = 0;
                    let max_empty_iterations = 5;
                    
                    let provider_box = match provider.latest() {
                        Ok(provider) => provider,
                        Err(e) => {
                            error!("[SGX DEBUG] Failed to get latest provider for NonceCache: {}", e);
                            return;
                        }
                    };
                    
                    let provider_arc = Arc::new(provider_box) as Arc<dyn StateProvider>;
                    let nonce_cache = NonceCache::new(provider_arc);
                    
                    let mut order_intake_consumer =
                        OrderIntakeConsumer::<self::ValidOrderPriority>::new(
                            nonce_cache,
                            order_receiver
                        );
                    
                    loop {
                        let sleep_duration = if order_batch_count >= max_empty_iterations {
                            Duration::from_millis(100)
                        } else {
                            let backoff_factor = std::cmp::min(order_batch_count, 5) as u64;
                            Duration::from_millis(500 * (1 + backoff_factor))
                        };
                        
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                info!("[SGX DEBUG] Cancellation received, stopping SGX WASM Builder");
                                break;
                            }
                            
                            _ = tokio::time::sleep(sleep_duration) => {
                                let ordering_id = OrderingId(format!("sgx-wasm-{}", uuid::Uuid::new_v4()));
                                order_batch_count += 1;
                                
                                let force_build = order_batch_count >= max_empty_iterations;
                                if force_build {
                                    info!("[SGX DEBUG] Force block building after {} empty iterations", max_empty_iterations);
                                }
                                
                                if order_batch_count % 2 == 0 {
                                    debug!("[SGX DEBUG] Iteration #{}/{}: Getting latest state provider", 
                                           order_batch_count, max_empty_iterations);
                                } else {
                                    trace!("[SGX DEBUG] Iteration #{}/{}: Getting latest state provider", 
                                           order_batch_count, max_empty_iterations);
                                }
                                
                                let state_provider = match provider.latest() {
                                    Ok(provider) => {
                                        trace!("[SGX DEBUG] Successfully obtained state provider");
                                        Arc::from(provider) as Arc<dyn StateProvider>
                                    },
                                    Err(e) => {
                                        error!("[SGX DEBUG] Failed to get latest state provider: {}", e);
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        continue;
                                    }
                                };
                                
                                trace!("[SGX DEBUG] Creating block building helper");
                                let helper = match block_building_helper::BlockBuildingHelperFromProvider::new(
                                    state_provider.clone(),
                                    ctx.clone(),
                                    None,
                                    name.clone(),
                                    true,
                                    cancel.clone(), 
                                ) {
                                    Ok(helper) => {
                                        trace!("[SGX DEBUG] Successfully created block building helper");
                                        helper
                                    },
                                    Err(e) => {
                                        error!("[SGX DEBUG] Failed to create block building helper: {}", e);
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        continue;
                                    }
                                };
                            
                                trace!("[SGX DEBUG] Attempting to consume next batch of orders");
                                match order_intake_consumer.try_consume_next_batch() {
                                    Ok(true) => {
                                        order_batch_count = 0;
                                        debug!("[SGX DEBUG] Successfully processed batch. Order count: {}", 
                                            order_intake_consumer.block_orders.get_all_orders().len());
                                    },
                                    Ok(false) => {
                                        info!("[SGX DEBUG] Order consumer channel closed, stopping SGX WASM builder");
                                        cancel.cancel();
                                        break;
                                    },
                                    Err(e) => {
                                        error!("[SGX DEBUG] Failed to consume batch of orders: {}", e);
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        continue;
                                    }
                                };
                                
                                if order_intake_consumer.block_orders.is_empty() && !force_build {
                                    if order_batch_count % 2 == 0 {
                                        debug!("[SGX DEBUG] No validated orders to process (iteration {}/{})", 
                                               order_batch_count, max_empty_iterations);
                                    } else {
                                        trace!("[SGX DEBUG] No validated orders to process (iteration {}/{})", 
                                               order_batch_count, max_empty_iterations);
                                    }
                                    continue;
                                }
                            
                                let block_orders = order_intake_consumer.current_block_orders();
                                
                                if block_orders.is_empty() && !force_build {
                                    debug!("[SGX DEBUG] No block orders available (iteration {}/{})", 
                                           order_batch_count, max_empty_iterations);
                                    continue;
                                }
                                
                                if force_build && block_orders.is_empty() {
                                    info!("[SGX DEBUG] Building empty block to avoid test timeout");
                                    order_batch_count = 0;
                                }
                                
                                debug!("[SGX DEBUG] Current block orders count: {}", block_orders.get_all_orders().len());
                                for (i, order) in block_orders.get_all_orders().iter().take(5).enumerate() {
                                    debug!("[SGX DEBUG] Order #{}: id={}, type={:?}", 
                                        i, order.id(), 
                                        match &order.order {
                                            crate::primitives::Order::Tx(_) => "Transaction",
                                            crate::primitives::Order::Bundle(_) => "Bundle",
                                            crate::primitives::Order::ShareBundle(_) => "ShareBundle",
                                        }
                                    );
                                }
                                
                                info!("[SGX DEBUG] Found {} orders to process, proceeding with block building", 
                                    block_orders.get_all_orders().len());
                                
                                #[cfg(feature = "sgx_integration")]
                                {
                                    info!("[SGX DEBUG] Building block using SGX enclave with {} orders", block_orders.get_all_orders().len());
                            
                                    let mut addresses = Vec::new();
                                    let mut access_list_entries: std::collections::HashMap<Address, std::collections::HashSet<B256>> =
                                        std::collections::HashMap::new();
                            
                                    debug!("[SGX DEBUG] Processing orders to extract addresses and access lists");
                                    for (i, order) in block_orders.get_all_orders().iter().enumerate() {
                                        debug!("[SGX DEBUG] Processing order #{} (id={})", i, order.id());
                                        match &order.order {
                                            Order::Tx(tx) => {
                                                debug!("[SGX DEBUG] Order #{} is a transaction", i);
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
                                                debug!("[SGX DEBUG] Order #{} is a bundle with {} transactions", i, bundle.list_txs().len());
                                                for (tx_idx, (tx, _)) in bundle.list_txs().iter().enumerate() {
                                                    debug!("[SGX DEBUG] Processing bundle tx #{} in order #{}", tx_idx, i);
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
                                                debug!("[SGX DEBUG] Order #{} is a share bundle with {} transactions", i, share_bundle.list_txs().len());
                                                for (tx_idx, (tx, _)) in share_bundle.list_txs().iter().enumerate() {
                                                    debug!("[SGX DEBUG] Processing share bundle tx #{} in order #{}", tx_idx, i);
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
                                        debug!(
                                            "Collected access list entries for {} addresses with a total of {} storage keys",
                                            access_list_entries.len(),
                                            access_list_entries.values().map(|keys| keys.len()).sum::<usize>()
                                        );
                                    }
                            
                                    debug!("Extracting state data for {} addresses", addresses.len());
                            
                                    if !access_list_entries.is_empty() {
                                        debug!("Including {} storage keys from transaction access lists",
                                            access_list_entries.values().map(|keys| keys.len()).sum::<usize>());
                                    }
                            
                                    addresses.sort();
                                    addresses.dedup();
                                    let block_number = ctx.evm_env.block_env.number.to::<u64>();
                                    let (accounts, mut storage, code) = owned_self.extract_state_data(&provider, block_number, &addresses)
                                        .unwrap_or_else(|e| {
                                            error!("Failed to extract state data: {}", e);
                                            (Vec::new(), Vec::new(), Vec::new())
                                        });
                            
                                    debug!("Extracted {} accounts, {} storage slots, and {} code entries",
                                        accounts.len(), storage.len(), code.len());
                            
                                    let mut additional_storage = Vec::new();
                                    for (address, keys) in &access_list_entries {
                                        for &key in keys {
                                            if let Ok(Some(value)) = state_provider.storage(*address, key) {
                                                additional_storage.push(SerializedStorage {
                                                    address: *address,
                                                    slot: key,
                                                    value: value.into(),
                                                });
                                            }
                                        }
                                    }
                            
                                    if !additional_storage.is_empty() {
                                        debug!("Adding {} additional storage slots from access lists", additional_storage.len());
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
                            
                                    debug!("[SGX DEBUG] Serializing block data to JSON");
                                    let orders_json = match serde_json::to_string(&serialized_data) {
                                        Ok(json) => {
                                            debug!("[SGX DEBUG] Successfully serialized orders to JSON: {} bytes", json.len());
                                            json
                                        },
                                        Err(e) => {
                                            error!("[SGX DEBUG] Failed to serialize orders for SGX: {}", e);
                                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                            continue;
                                        }
                                    };
                                    
                                    debug!("[SGX DEBUG] Calling SGX build_block with input JSON of size: {} bytes", orders_json.len());
                                            debug!("[SGX DEBUG] Input JSON preview: {}", orders_json);

                                    match owned_self.sgx_builder.build_block(&orders_json) {
                                        Ok(output_json) => {
                                            debug!("[SGX DEBUG] SGX block building completed successfully, output size: {} bytes", output_json.len());
                                            
                                            debug!("[SGX DEBUG] Starting signature verification");
                                            debug!("[SGX DEBUG] Output JSON preview: {}", output_json);
                                            match owned_self.verifier.verify(&output_json) {
                                                Ok(_) => {
                                                    debug!("[SGX DEBUG] Successfully verified SGX enclave signature");
                                                    debug!("[SGX DEBUG] Parsing output JSON into BlockBuilderOutput");
                                                    match serde_json::from_str::<BlockBuilderOutput>(&output_json) {
                                                        Ok(output) => {
                                                            debug!("[SGX DEBUG] Successfully parsed SGX output JSON");
                                                            info!("[SGX DEBUG] Metrics from SGX enclave output: block_value={}, tx_count={}, blob_count={}, gas_used={}, blob_gas_used={}, orders_included={}, orders_failed={}, build_time_us={}", 
                                                                output.metrics.block_value, 
                                                                output.metrics.tx_count,
                                                                output.metrics.blob_count,
                                                                output.metrics.gas_used,
                                                                output.metrics.blob_gas_used.unwrap_or_default(),
                                                                output.metrics.trace.as_ref().map_or(0, |t| t.orders_included),
                                                                output.metrics.trace.as_ref().map_or(0, |t| t.orders_failed),
                                                                output.metrics.build_time_us);
                                                            
                                                            if let Some(trace) = &output.metrics.trace {
                                                                crate::telemetry::add_block_fill_time(
                                                                    Duration::from_micros(trace.sim_time_us),
                                                                    "SGX-WASM-Builder",
                                                                    ctx.timestamp()
                                                                );
                                                                
                                                                add_histogram_value_in_tests(
                                                                    "sgx_wasm_builder.sim_time_us",
                                                                    trace.sim_time_us as f64
                                                                );
                                                                add_histogram_value_in_tests(
                                                                    "sgx_wasm_builder.root_hash_time_us", 
                                                                    trace.root_hash_time_us as f64
                                                                );
                                                                add_histogram_value_in_tests(
                                                                    "sgx_wasm_builder.finalize_time_us", 
                                                                    trace.finalize_time_us as f64
                                                                );
                                                                add_histogram_value_in_tests(
                                                                    "sgx_wasm_builder.ordering_time_us", 
                                                                    trace.ordering_time_us as f64
                                                                );
                                                                
                                                                add_counter_value_in_tests(
                                                                    "sgx_wasm_builder.orders_considered", 
                                                                    trace.orders_considered as u64
                                                                );
                                                                add_counter_value_in_tests(
                                                                    "sgx_wasm_builder.orders_included", 
                                                                    trace.orders_included as u64
                                                                );
                                                                add_counter_value_in_tests(
                                                                    "sgx_wasm_builder.orders_failed", 
                                                                    trace.orders_failed as u64
                                                                );
                                                            }
                                                            
                                                            let block_value_f64 = output.metrics.block_value.to_string()
                                                                .parse::<f64>()
                                                                .unwrap_or(0.0);
                                                            
                                                            add_histogram_value_in_tests(
                                                                "sgx_wasm_builder.block_value", 
                                                                block_value_f64
                                                            );
                                                            add_counter_value_in_tests(
                                                                "sgx_wasm_builder.gas_used", 
                                                                output.metrics.gas_used
                                                            );
                                                            add_counter_value_in_tests(
                                                                "sgx_wasm_builder.tx_count", 
                                                                output.metrics.tx_count as u64
                                                            );
                                                            
                                                            debug!("[SGX DEBUG] Converting SGX output to block");
                                                            match owned_self.convert_output_to_block(output, &helper, &ctx) {
                                                                Ok(block) => {
                                                                    debug!("[SGX DEBUG] Successfully converted SGX output to block");
                                                                    let block_value = block.true_block_value();
                                                                    info!("[SGX DEBUG] Built block with value: {}", block_value);
                                                                    
                                                                    info!("[SGX DEBUG] Sending block directly to sink");
                                                                    let block_value_str = block.true_block_value().to_string();
                                                                    tracing::info!(
                                                                        "[SGX DEBUG] About to send block with value {} to sink (type: {})",
                                                                        block_value_str,
                                                                        std::any::type_name_of_val(&sink)
                                                                    );
                                                                    sink.new_block(block);
                                                                    debug!("[SGX DEBUG] Block sent to sink");
                                                                    return;
                                                                },
                                                                Err(e) => {
                                                                    error!("[SGX DEBUG] Failed to convert SGX output to block: {}", e);
                                                                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                                                    continue;
                                                                }
                                                            }
                                                        },
                                                        Err(e) => {
                                                            error!("[SGX DEBUG] Failed to parse SGX output: {}", e);
                                                            error!("[SGX DEBUG] SGX output JSON preview (first 100 chars): {}", 
                                                                output_json.chars().take(100).collect::<String>());
                                                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                                            continue;
                                                        }
                                                    }
                                                },
                                                Err(e) => {
                                                    error!("[SGX DEBUG] SGX signature verification failed: {}", e);
                                                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                                    continue;
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            error!("[SGX DEBUG] Failed to build block with SGX: {}", e);
                                            
                                            debug!("[SGX DEBUG] SGX build error occurred, serialized_data: accounts={}, storage={}, code={}, transactions={}, bundles={}", 
                                                serialized_data.accounts.len(),
                                                serialized_data.storage.len(),
                                                serialized_data.code.len(),
                                                serialized_data.transactions.len(),
                                                serialized_data.bundles.len()
                                            );
                                            
                                            if fallback_to_native {
                                                warn!("[SGX DEBUG] Falling back to native block building due to SGX error");
                                                debug!("[SGX DEBUG] Creating fallback block with SgxWasmBlockBuildingHelper");
                                                let fallback_helper = SgxWasmBlockBuildingHelper::new(
                                                    format!("{}-fallback", name),
                                                    U256::from(0),
                                                    ctx.clone(),
                                                    Vec::new(),
                                                    Vec::new(),
                                                    Vec::new(),
                                                    Vec::new(),
                                                    SerializedHeader {
                                                        parent_hash: B256::default(),
                                                        number: 0,
                                                        timestamp: 0,
                                                        coinbase: Address::default(),
                                                        difficulty: alloy_primitives::U256::ZERO,
                                                        gas_limit: 0,
                                                        gas_used: 0,
                                                        base_fee_per_gas: alloy_primitives::U256::ZERO,
                                                        extra_data: alloy_primitives::Bytes::default(),
                                                        state_root: B256::default(),
                                                        transactions_root: B256::default(),
                                                        receipts_root: B256::default(),
                                                        logs_bloom: [0u8; 256],
                                                        mix_hash: B256::default(),
                                                        withdrawals_root: None,
                                                        blob_gas_used: None,
                                                        excess_blob_gas: None,
                                                    },
                                                    SerializedStateDiff {
                                                        accounts: Vec::new(),
                                                        storage: Vec::new(),
                                                        code: Vec::new(),
                                                    },
                                                );
                                                debug!("[SGX DEBUG] Initializing BiddableUnfinishedBlock with fallback helper");
                                                let fallback_block = match block_building_helper::BiddableUnfinishedBlock::new(Box::new(fallback_helper)) {
                                                    Ok(block) => {
                                                        debug!("[SGX DEBUG] Successfully created fallback BiddableUnfinishedBlock");
                                                        block
                                                    },
                                                    Err(e) => {
                                                        error!("[SGX DEBUG] Failed to create fallback BiddableUnfinishedBlock: {}", e);
                                                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                                        continue;
                                                    }
                                                };
                                                info!("[SGX DEBUG] Using fallback block directly");
                                                
                                                let block_value = fallback_block.true_block_value();
                                                
                                                info!("[SGX DEBUG] Fallback block with value: {}", block_value);
                                                debug!("[SGX DEBUG] Sending fallback block directly to sink");
                                                
                                                std::thread::sleep(std::time::Duration::from_millis(100));

                                                sink.new_block(fallback_block);
                                                debug!("[SGX DEBUG] Fallback block sent to sink");
                                                return;
                                            } else {
                                                debug!("[SGX DEBUG] No fallback to native option, continuing without creating a block");
                                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            });
        }

        {
            let cancel = cancel_token.clone();
            let join_set_clone = join_set.clone();
            tokio::spawn(async move {
                let mut join_set = join_set_clone.lock().await;
                join_set.spawn(async move {
                    let cancel = cancel.clone();
                    
                    info!("[SGX DEBUG] Starting order forwarding task");
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                info!("[SGX DEBUG] Cancellation received, stopping order forwarding task");
                                break;
                            }
                            
                            cmd_result = input_receiver.recv() => {
                                match cmd_result {
                                    Ok(cmd) => {
                                        match order_sender.send(cmd) {
                                            Ok(_) => {},
                                            Err(tokio::sync::broadcast::error::SendError(cmd)) => {
                                                warn!("[SGX DEBUG] Failed to forward order (no receivers): {:?}", cmd);
                                                if order_sender.receiver_count() == 0 {
                                                    warn!("[SGX DEBUG] All receivers were dropped, creating a new one");
                                                    let _ = order_sender.subscribe();
                                                }
                                            }
                                        }
                                    },
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                        info!("[SGX DEBUG] Input channel closed, stopping order forwarding task");
                                        cancel.cancel();
                                        break;
                                    },
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                        warn!("[SGX DEBUG] Message forwarding lagged by {} messages", n);
                                    }
                                }
                            }
                        }
                    }
                    info!("[SGX DEBUG] Order forwarding task terminated");
                });
            });
        }

        let cancel = cancel_token.clone();
        let join_set_clone = join_set.clone();
        
        tokio::spawn(async move {
            cancel.cancelled().await;
            info!("[SGX DEBUG] Cancellation received, cleaning up tasks");
            
            let mut join_set_guard = join_set_clone.lock().await;
            
            join_set_guard.abort_all();
            while let Some(res) = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                join_set_guard.join_next()
            ).await.ok().flatten() {
                if let Err(e) = res {
                    if e.is_cancelled() {
                        debug!("[SGX DEBUG] Task was properly cancelled");
                    } else {
                        warn!("[SGX DEBUG] Task failed during cleanup: {}", e);
                    }
                }
            }
            
            info!("[SGX DEBUG] All tasks cleaned up successfully");
        });
    }
}
