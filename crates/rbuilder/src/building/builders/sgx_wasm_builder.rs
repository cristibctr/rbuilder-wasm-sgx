use std::fmt::Debug;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::collections::HashMap;

use alloy_primitives::{Address, Bytes, StorageValue, B256, U256 as AlloyU256, keccak256, FixedBytes};
use alloy_rlp::{Encodable as AlloyEncodable, Decodable as AlloyDecodable, RlpEncodable, RlpDecodable};
use alloy_eips::eip2718::Encodable2718;
use eyre::{eyre, Result, Report as ErrReport};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error, debug, trace};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use std::hash::Hash;
use alloy_consensus::{Transaction, TxType, EMPTY_ROOT_HASH, EMPTY_OMMER_ROOT_HASH, Header};
use alloy_eips::merge::BEACON_NONCE;
use alloy_eips::eip4895::Withdrawal;
use reth_node_api::{FullNodeComponents, FullNodeTypes, PayloadBuilderAttributes};
use reth_provider::{AccountReader, StateProvider, StateProviderFactory};
use reth_errors::ProviderError;
use reth_transaction_pool::PoolTransaction;
use reth_primitives_traits::{SignedTransaction, Account};
use reth_primitives::{
    TransactionSigned, Receipt as RethReceipt, Log as RethLog, LogData,
};
use alloy_primitives::Bloom;
use crate::building::{Address as RethAddress, Bytes as RethBytes, B256 as RethB256, U256 as RethU256};
use block_builder_types::{ExecutionMode, SgxOrderingInput, SgxOrderingResult, SgxOrderingOutput, OrderForOrdering, OrderedTransaction};
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
        BlockBuildingAlgorithm, BlockBuildingAlgorithmInput, BlockBuildingContext, UnfinishedBlockBuildingSink,
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
use alloy_consensus::constants::{EMPTY_WITHDRAWALS, KECCAK_EMPTY};

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
    sgx_output_header: Header,
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
        sgx_output_header: Header,
        sgx_state_diff: SerializedStateDiff,
        block_trace: BuiltBlockTrace,
    ) -> Self {
        Self {
            block_value,
            builder_name,
            block_trace,
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

        let header = self.sgx_output_header.clone();
        
        debug!("[SGX DEBUG] SGX Header fields: parent_hash={:?}, number={}, timestamp={}, gas_limit={}, gas_used={}, base_fee_per_gas={:?}, beneficiary={:?}, state_root={:?}, transactions_root={:?}, receipts_root={:?}, mix_hash={:?}, withdrawals_root={:?}, blob_gas_used={:?}, excess_blob_gas={:?}",
            header.parent_hash, header.number, header.timestamp, header.gas_limit, header.gas_used, header.base_fee_per_gas, header.beneficiary, header.state_root, header.transactions_root, header.receipts_root, header.mix_hash, header.withdrawals_root, header.blob_gas_used, header.excess_blob_gas);
        debug!("[SGX DEBUG] Context prev_randao={:?}", ctx.attributes.prev_randao);
        
        self.block_trace.root_hash_time = Duration::from_millis(0);
        
        let reth_transactions: Vec<TransactionSigned> = self.executed_txs.iter()
            .map(|tx| tx.internal_tx_unsecure().tx().clone())
            .collect();

        let withdrawals = if ctx.chain_spec.is_shanghai_active_at_timestamp(header.timestamp) {
            Some(ctx.attributes.withdrawals.clone())
        } else {
            None
        };
        
        let body = reth::primitives::BlockBody {
            transactions: reth_transactions,
            ommers: vec![],
            withdrawals,
        };
        
        let block_with_senders = reth::primitives::Block { header: header.clone(), body };
        
        let block_header = &block_with_senders.header;
        debug!("[SGX DEBUG] === HEADER COMPARISON ===");
        debug!("[SGX DEBUG] SGX parent_hash: {:?} vs Block parent_hash: {:?}", header.parent_hash, block_header.parent_hash);
        debug!("[SGX DEBUG] SGX ommers_hash: {:?} vs Block ommers_hash: {:?}", header.ommers_hash, block_header.ommers_hash);
        debug!("[SGX DEBUG] SGX beneficiary: {:?} vs Block beneficiary: {:?}", header.beneficiary, block_header.beneficiary);
        debug!("[SGX DEBUG] SGX state_root: {:?} vs Block state_root: {:?}", header.state_root, block_header.state_root);
        debug!("[SGX DEBUG] SGX transactions_root: {:?} vs Block transactions_root: {:?}", header.transactions_root, block_header.transactions_root);
        debug!("[SGX DEBUG] SGX receipts_root: {:?} vs Block receipts_root: {:?}", header.receipts_root, block_header.receipts_root);
        debug!("[SGX DEBUG] SGX logs_bloom: {:?} vs Block logs_bloom: {:?}", header.logs_bloom, block_header.logs_bloom);
        debug!("[SGX DEBUG] SGX difficulty: {:?} vs Block difficulty: {:?}", header.difficulty, block_header.difficulty);
        debug!("[SGX DEBUG] SGX number: {:?} vs Block number: {:?}", header.number, block_header.number);
        debug!("[SGX DEBUG] SGX gas_limit: {:?} vs Block gas_limit: {:?}", header.gas_limit, block_header.gas_limit);
        debug!("[SGX DEBUG] SGX gas_used: {:?} vs Block gas_used: {:?}", header.gas_used, block_header.gas_used);
        debug!("[SGX DEBUG] SGX timestamp: {:?} vs Block timestamp: {:?}", header.timestamp, block_header.timestamp);
        debug!("[SGX DEBUG] SGX extra_data: {:?} vs Block extra_data: {:?}", header.extra_data, block_header.extra_data);
        debug!("[SGX DEBUG] SGX mix_hash: {:?} vs Block mix_hash: {:?}", header.mix_hash, block_header.mix_hash);
        debug!("[SGX DEBUG] SGX nonce: {:?} vs Block nonce: {:?}", header.nonce, block_header.nonce);
        debug!("[SGX DEBUG] SGX base_fee_per_gas: {:?} vs Block base_fee_per_gas: {:?}", header.base_fee_per_gas, block_header.base_fee_per_gas);
        debug!("[SGX DEBUG] SGX withdrawals_root: {:?} vs Block withdrawals_root: {:?}", header.withdrawals_root, block_header.withdrawals_root);
        debug!("[SGX DEBUG] SGX blob_gas_used: {:?} vs Block blob_gas_used: {:?}", header.blob_gas_used, block_header.blob_gas_used);
        debug!("[SGX DEBUG] SGX excess_blob_gas: {:?} vs Block excess_blob_gas: {:?}", header.excess_blob_gas, block_header.excess_blob_gas);
        debug!("[SGX DEBUG] SGX parent_beacon_block_root: {:?} vs Block parent_beacon_block_root: {:?}", header.parent_beacon_block_root, block_header.parent_beacon_block_root);
        debug!("[SGX DEBUG] === END COMPARISON ===");

        let sgx_hash = header.hash_slow();
        let block_hash = block_header.hash_slow();
        debug!("[SGX DEBUG] SGX computed hash: {:?}", sgx_hash);
        debug!("[SGX DEBUG] Block header computed hash: {:?}", block_hash);
        
        let sealed_block = reth::primitives::SealedBlock::seal_slow(block_with_senders);
        
        debug!("[SGX DEBUG] Final sealed block hash: {:?}", sealed_block.hash());
        
        self.block_trace.finalize_time = start_time.elapsed();
        
        info!(
            "SGX Block finalized with SGX-computed header: block={}, gas_used={}, tx_count={}, blob_count={}, value={}, finalize_time={:?}, block_hash={:?}",
            self.sgx_output_header.number,
            self.sgx_output_header.gas_used,
            self.executed_txs.len(),
            self.blob_sidecars.len(),
            self.block_value,
            self.block_trace.finalize_time,
            sealed_block.hash()
        );
        
        Ok(FinalizeBlockResult {
            block: crate::building::builders::Block {
                trace: self.block_trace.clone(),
                sealed_block,
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
use block_builder_types::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockBuilderInput {
    block_params: BlockParams,
    accounts: Vec<SerializedAccount>,
    storage: Vec<SerializedStorage>,
    code: Vec<SerializedCode>,
    transactions: Vec<SerializedTransaction>,
    bundles: Vec<SerializedBundle>,
    withdrawals: Vec<SerializedWithdrawal>,
    config: BlockBuilderConfig,
}
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
    sgx_builder: BlockBuilderSgx,

    verifier: BlockSignatureVerifier,
    wasm_path: PathBuf,
    fallback_to_native: bool,
    
    execution_mode: ExecutionMode,
}

impl SgxWasmBlockBuildingAlgorithm {
    pub fn new(wasm_path: PathBuf, fallback_to_native: bool) -> Result<Self> {
        Self::new_with_execution_mode(wasm_path, fallback_to_native, ExecutionMode::default())
    }
    
    pub fn new_with_execution_mode(
        wasm_path: PathBuf, 
        fallback_to_native: bool,
        execution_mode: ExecutionMode,
    ) -> Result<Self> {

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
        
        info!("[SGX DEBUG] Execution mode: {:?}", execution_mode);
        
        Ok(Self {
            sgx_builder,
            verifier,
            wasm_path,
            fallback_to_native,
            execution_mode,
        })
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
                        for i in 4..20 {  
                            storage_keys.push(B256::with_last_byte(i));
                        }
                    }
                }

                if account.bytecode_hash.is_none() || account.bytecode_hash == Some(B256::ZERO) {
                    for i in 20..50 {
                        storage_keys.push(B256::with_last_byte(i));
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
                    
                    let mut encoded_bytes = Vec::new();
                    tx.internal_tx_unsecure().encode_2718(&mut encoded_bytes);
                    let encoded_signed_tx = alloy_primitives::Bytes::from(encoded_bytes);

                    let serialized_tx = self.serialize_transaction(inner_tx, from, hash, encoded_signed_tx);
                    transactions.push(serialized_tx);
                },

                crate::primitives::Order::Bundle(bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();
                        
                        let mut encoded_bytes = Vec::new();
                        tx.internal_tx_unsecure().encode_2718(&mut encoded_bytes);
                        let encoded_signed_tx = alloy_primitives::Bytes::from(encoded_bytes);

                        let serialized_tx = self.serialize_transaction(inner_tx, from, hash, encoded_signed_tx);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                id: order_id,
                transactions: bundle_transactions,
                hash: bundle.hash,
                revertible: !bundle.reverting_tx_hashes.is_empty(),
                        });
                    }
                },

                crate::primitives::Order::ShareBundle(share_bundle) => {
                    let mut bundle_transactions = Vec::new();

                    for (tx, _) in share_bundle.list_txs() {
                        let inner_tx = tx.internal_tx_unsecure().transaction();
                        let from = tx.signer();
                        let hash = tx.hash();
                        
                        let mut encoded_bytes = Vec::new();
                        tx.internal_tx_unsecure().encode_2718(&mut encoded_bytes);
                        let encoded_signed_tx = alloy_primitives::Bytes::from(encoded_bytes);

                        let serialized_tx = self.serialize_transaction(inner_tx, from, hash, encoded_signed_tx);
                        bundle_transactions.push(serialized_tx);
                    }

                    if !bundle_transactions.is_empty() {
                        bundles.push(SerializedBundle {
                id: order_id,
                transactions: bundle_transactions,
                hash: share_bundle.hash,
                revertible: share_bundle.inner_bundle().can_skip,
                        });
                    }
                }
            }
        }

        (transactions, bundles)
    }
    fn serialize_transaction(&self, tx: &reth_primitives::Transaction, sender: Address, hash: B256, encoded_signed_tx: alloy_primitives::Bytes) -> SerializedTransaction {
        let tx_type = tx.tx_type();
        let access_list = if let Some(al) = tx.access_list() {
            al.0.clone()
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
            tx_type,
            access_list,
            blob_hashes: Vec::new(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas().map(|fee| U256::from(fee)),
            max_fee_per_gas: Some(U256::from(tx.max_fee_per_gas())),
            max_fee_per_blob_gas: tx.max_fee_per_blob_gas().map(|fee| U256::from(fee)),
            versioned_hashes: Vec::new(),
            encoded_signed_tx,
        }
    }
    fn convert_block_params(&self, ctx: &BlockBuildingContext) -> BlockParams {
        let (blob_gas_used, excess_blob_gas) = if ctx.chain_spec.is_cancun_active_at_timestamp(ctx.attributes.timestamp) {
            (Some(0), ctx.excess_blob_gas)
        } else {
            (None, None)
        };

        let withdrawals_root = if ctx.chain_spec.is_shanghai_active_at_timestamp(ctx.attributes.timestamp) {
            use reth_basic_payload_builder::commit_withdrawals;
            use revm::State;
            use revm::db::EmptyDB;
            
            let mut db = State::builder()
                .with_database(EmptyDB::default())
                .with_bundle_update()
                .build();
                
            match commit_withdrawals(
                &mut db,
                &ctx.chain_spec,
                ctx.attributes.timestamp,
                &ctx.attributes.withdrawals,
            ) {
                Ok(root) => {
                    debug!("[SGX DEBUG] Computed withdrawals_root using commit_withdrawals: {:?}", root);
                    root
                },
                Err(e) => {
                    debug!("[SGX DEBUG] commit_withdrawals failed: {}, using empty root", e);
                    Some(reth_trie::EMPTY_ROOT_HASH)
                }
            }
        } else {
            None
        };

        BlockParams {
            number: ctx.evm_env.block_env.number.to::<u64>(),
            timestamp: ctx.attributes.timestamp,
            gas_limit: ctx.evm_env.block_env.gas_limit.to::<u64>(),
            base_fee_per_gas: ctx.evm_env.block_env.basefee,
            coinbase: ctx.attributes.suggested_fee_recipient,
            parent_hash: ctx.attributes.parent,
            parent_state_root: B256::default(),
            withdrawals_root,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_block_root: ctx.attributes.parent_beacon_block_root,
            prev_randao: ctx.attributes.prev_randao,
        }
    }
    fn convert_withdrawals(&self, ctx: &BlockBuildingContext) -> Vec<SerializedWithdrawal> {
        if ctx.chain_spec.is_shanghai_active_at_timestamp(ctx.attributes.timestamp) {
            ctx.attributes.withdrawals.to_vec()
        } else {
            Vec::new()
        }
    }

    fn create_config(&self, ctx: &BlockBuildingContext) -> BlockBuilderConfig {
        BlockBuilderConfig {
            discard_txs: true,
            sorting: block_builder_types::SortingAlgorithm::MevGasPrice,
            failed_tx_retries: 1,
            drop_failed_txs: true,
            coinbase_payment: !ctx.coinbase_is_suggested_fee_recipient(),
            build_timeout_ms: 10000,
            complete_state_diff: false,
            include_merkle_proofs: false,
            compression_level: "medium".to_string(),
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
            match TransactionSigned::decode(&mut tx_bytes_slice) {
                Ok(tx_signed_reth) => {
                    let recover_result = tx_signed_reth.recover_signer();
                    match recover_result {
                        Ok(signer) => {
                let recovered = reth_primitives::Recovered::new_unchecked(tx_signed_reth, signer);
                let tx_with_blobs = crate::primitives::TransactionSignedEcRecoveredWithBlobs::new_no_blobs(recovered)
                    .map_err(|e| eyre!("Failed to create TransactionSignedEcRecoveredWithBlobs: {}", e))?;
                executed_txs.push(tx_with_blobs);
                        },
                        Err(e) => {
                warn!("Failed to recover signer for transaction from SGX output: {}", e);
                        }
                    }
                },
                Err(e) => {
                    warn!("Failed to decode transaction from SGX output: {:?}", tx_bytes_alloy);
                }
            }
        }
        
        for (i, receipt_data) in output.receipts.iter().enumerate() {
            let mut logs = Vec::new();
            for log_data in &receipt_data.logs {
                let reth_log_data = LogData::new_unchecked(
                    log_data.topics().iter().map(|t| RethB256::from_slice(t.as_slice())).collect(), 
                    RethBytes(log_data.data.data.clone().into())
                );
                logs.push(RethLog {
                    address: RethAddress::from_slice(log_data.address.as_slice()),
                    data: reth_log_data,
                });
            }
            
            let tx_type = if i < output.transactions.len() {
                let tx_bytes = &output.transactions[i];
                let mut tx_bytes_slice: &[u8] = &tx_bytes.0;
                match TransactionSigned::decode(&mut tx_bytes_slice) {
                    Ok(tx_signed_reth) => {
                        match tx_signed_reth.tx_type() {
                            reth_primitives::TxType::Legacy => alloy_consensus::TxType::Legacy,
                            reth_primitives::TxType::Eip2930 => alloy_consensus::TxType::Eip2930,
                            reth_primitives::TxType::Eip1559 => alloy_consensus::TxType::Eip1559,
                            reth_primitives::TxType::Eip4844 => alloy_consensus::TxType::Eip4844,
                            reth_primitives::TxType::Eip7702 => alloy_consensus::TxType::Eip7702,
                        }
                    },
                    Err(_) => {
                        warn!("Failed to decode transaction at index {} for receipt processing, using Legacy as fallback", i);
                        alloy_consensus::TxType::Legacy
                    }
                }
            } else {
                warn!("Receipt index {} exceeds transaction list length {}, using Legacy as fallback", i, output.transactions.len());
                alloy_consensus::TxType::Legacy
            };
            
            let reth_tx_type = match tx_type {
                alloy_consensus::TxType::Legacy => reth_primitives::TxType::Legacy,
                alloy_consensus::TxType::Eip2930 => reth_primitives::TxType::Eip2930,
                alloy_consensus::TxType::Eip1559 => reth_primitives::TxType::Eip1559,
                alloy_consensus::TxType::Eip4844 => reth_primitives::TxType::Eip4844,
                alloy_consensus::TxType::Eip7702 => {
                    warn!("EIP-7702 transaction type encountered, treating as EIP-1559 for reth compatibility");
                    reth_primitives::TxType::Eip1559
                },
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
            block_trace,
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

        let (sgx_builder, verifier) = {
            let sgx_builder = self.sgx_builder.clone();
            let verifier = self.verifier.clone();
            (sgx_builder, verifier)
        };

        let ctx = input.ctx.clone();
        let sink = input.sink.clone();
        let cancel = input.cancel.clone();
        let provider = input.provider.clone();

        let block_state: Arc<dyn StateProvider> = match input
            .provider
            .history_by_block_hash(input.ctx.attributes.parent)
        {
            Ok(state) => Arc::from(state),
            Err(err) => {
                error!(
                    ?err,
                    payload_id = input.ctx.payload_id,
                    "Failed to get history_by_block_hash, cancelling SGX WASM builder job"
                );
                return;
            }
        };

        let nonces = NonceCache::new(block_state.clone());
        let mut order_intake_consumer =
            OrderIntakeConsumer::<self::ValidOrderPriority>::new(nonces, input.input);

        let mut owned_self = SgxWasmBlockBuildingAlgorithm {
            sgx_builder,
            verifier,
            wasm_path: wasm_path.clone(),
            fallback_to_native,
            execution_mode: self.execution_mode.clone(),
        };

        loop {
            if cancel.is_cancelled() {
                info!("[SGX DEBUG] Cancellation received, stopping SGX WASM Builder");
                break;
            }

            trace!("[SGX DEBUG] Attempting to consume next batch of orders");
            match order_intake_consumer.blocking_consume_next_batch() {
                Ok(true) => {
                    debug!("[SGX DEBUG] Successfully processed batch. Order count: {}",
                        order_intake_consumer.block_orders.get_all_orders().len());
                },
                Ok(false) => {
                    info!("[SGX DEBUG] Order consumer channel closed, stopping SGX WASM builder");
                    break;
                },
                Err(e) => {
                    error!("[SGX DEBUG] Failed to consume batch of orders: {}", e);
                    continue;
                }
            };

            let block_orders = order_intake_consumer.current_block_orders();

            trace!("[SGX DEBUG] Getting latest state provider");
            let state_provider = match provider.latest() {
                Ok(provider) => {
                    trace!("[SGX DEBUG] Successfully obtained state provider");
                    Arc::from(provider) as Arc<dyn StateProvider>
                },
                Err(e) => {
                    error!("[SGX DEBUG] Failed to get latest state provider: {}", e);
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
                    continue;
                }
            };

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

            match owned_self.execution_mode {
                ExecutionMode::Legacy => {
                    info!("[SGX DEBUG] Using Legacy execution mode (static state prediction)");
                    owned_self.build_block_legacy_mode(&block_orders, &ctx, &provider, &state_provider, &helper, &sink, &name);
                },
                ExecutionMode::OrderingOnlyMode => {
                    info!("[SGX DEBUG] Using Ordering-Only Mode (host execution with SGX verification)");
                    owned_self.build_block_ordering_only_mode(&block_orders, &ctx, &provider, &state_provider, &helper, &sink, &name);
                }
            }
        }
    }

}

impl SgxWasmBlockBuildingAlgorithm {
    fn build_block_legacy_mode<P: provider::StateProviderFactory + Send + Sync + 'static + Clone>(
        &mut self,
        block_orders: &ValidatedOrderMap,
        ctx: &BlockBuildingContext,
        provider: &P,
        state_provider: &Arc<dyn StateProvider>,
        helper: &dyn BlockBuildingHelper,
        sink: &Arc<dyn UnfinishedBlockBuildingSink>,
        name: &str,
    ) {
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

        if ctx.chain_spec.is_shanghai_active_at_timestamp(ctx.attributes.timestamp) {
            let withdrawals = &ctx.attributes.withdrawals;
            debug!("[SGX DEBUG] Adding {} withdrawal recipients to address collection", withdrawals.len());
            for withdrawal in withdrawals {
                addresses.push(withdrawal.address);
                debug!("[SGX DEBUG] Added withdrawal recipient: {:?}", withdrawal.address);
            }
        }


        debug!("Extracting state data for {} addresses", addresses.len());

        if !access_list_entries.is_empty() {
            debug!("Including {} storage keys from transaction access lists",
                access_list_entries.values().map(|keys| keys.len()).sum::<usize>());
        }

        addresses.sort();
        addresses.dedup();
        let block_number = ctx.evm_env.block_env.number.to::<u64>();
        let (accounts, mut storage, code) = self.extract_state_data(provider, block_number, &addresses)
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
            block_params: self.convert_block_params(&ctx),
            accounts,
            storage,
            code,
            transactions: Vec::new(),
            bundles: Vec::new(),
            withdrawals: self.convert_withdrawals(&ctx),
            config: self.create_config(&ctx),
        };

        let (transactions, bundles) = self.convert_orders(&block_orders);
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
                return;
            }
        };

        debug!("[SGX DEBUG] Calling SGX build_block with input JSON of size: {} bytes", orders_json.len());
        debug!("[SGX DEBUG] Input JSON preview: {}", orders_json);

        match self.sgx_builder.build_block(&orders_json) {
            Ok(output_json) => {
                debug!("[SGX DEBUG] SGX block building completed successfully, output size: {} bytes", output_json.len());

                debug!("[SGX DEBUG] Starting signature verification");
                debug!("[SGX DEBUG] Output JSON preview: {}", output_json);
                match self.verifier.verify(&output_json) {
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
                                match self.convert_output_to_block(output, helper, &ctx) {
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
                                        return;
                                    }
                                }
                            },
                            Err(e) => {
                                error!("[SGX DEBUG] Failed to parse SGX output: {}", e);
                                error!("[SGX DEBUG] SGX output JSON preview (first 100 chars): {}",
                                    output_json.chars().take(100).collect::<String>());
                                return;
                            }
                        }
                    },
                    Err(e) => {
                        error!("[SGX DEBUG] SGX signature verification failed: {}", e);
                        return;
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
                return;
            }
        }
    }

    fn build_block_ordering_only_mode<P: provider::StateProviderFactory + Send + Sync + 'static + Clone>(
        &mut self,
        block_orders: &ValidatedOrderMap,
        ctx: &BlockBuildingContext,
        provider: &P,
        state_provider: &Arc<dyn StateProvider>,
        helper: &dyn BlockBuildingHelper,
        sink: &Arc<dyn UnfinishedBlockBuildingSink>,
        name: &str,
    ) {
        info!("[Ordering-Only Mode] Executing Ordering-Only Mode: Host collection + SGX ordering + Host execution");
        
        info!("[Ordering-Only Mode] Phase 1: Collecting orders for SGX ordering");
        let ordering_input = match self.prepare_sgx_ordering_input(block_orders, ctx) {
            Ok(input) => {
                info!("[Ordering-Only Mode] Prepared {} orders for SGX ordering", input.orders.len());
                input
            },
            Err(e) => {
                error!("[Ordering-Only Mode] Failed to prepare SGX ordering input: {}", e);
                if self.fallback_to_native {
                    warn!("[Ordering-Only Mode] Falling back to legacy mode");
                    return self.build_block_legacy_mode(block_orders, ctx, provider, state_provider, helper, sink, name);
                } else {
                    return;
                }
            }
        };

        info!("[Ordering-Only Mode] Phase 2: SGX determining trusted transaction ordering");
        let ordering_result = match self.get_sgx_transaction_ordering(&ordering_input) {
            Ok(result) => {
                info!("[Ordering-Only Mode] SGX ordering completed: {} transactions ordered", result.ordered_transactions.len());
                result
            },
            Err(e) => {
                error!("[Ordering-Only Mode] SGX ordering failed: {}", e);
                if self.fallback_to_native {
                    warn!("[Ordering-Only Mode] Falling back to legacy mode due to SGX ordering failure");
                    return self.build_block_legacy_mode(block_orders, ctx, provider, state_provider, helper, sink, name);
                } else {
                    return;
                }
            }
        };

        info!("[Ordering-Only Mode] Phase 4: Verifying SGX signature on ordering decision");
        match self.verify_sgx_ordering_signature(&ordering_result) {
            Ok(_) => {
                info!("[Ordering-Only Mode] SGX ordering signature verified - MEV protection confirmed");
            },
            Err(e) => {
                error!("[Ordering-Only Mode] SGX ordering signature verification failed: {}", e);
                if self.fallback_to_native {
                    warn!("[Ordering-Only Mode] Proceeding without SGX verification due to signature failure");
                }
            }
        }

        info!("[Ordering-Only Mode] Phase 3: Executing SGX-ordered transactions with full Reth infrastructure");
        let final_block = match self.execute_sgx_ordered_transactions(
            &ordering_result, block_orders, ctx, provider, state_provider, name
        ) {
            Ok(block) => {
                info!("[Ordering-Only Mode] Host execution of SGX-ordered transactions completed, block value: {}", block.true_block_value());
                block
            },
            Err(e) => {
                error!("[Ordering-Only Mode] Host execution of ordered transactions failed: {}", e);
                if self.fallback_to_native {
                    warn!("[Ordering-Only Mode] Falling back to legacy mode due to execution failure");
                    return self.build_block_legacy_mode(block_orders, ctx, provider, state_provider, helper, sink, name);
                } else {
                    return;
                }
            }
        };
        sink.new_block(final_block);
    }

    fn prepare_sgx_ordering_input(
        &self,
        block_orders: &ValidatedOrderMap,
        ctx: &BlockBuildingContext,
    ) -> Result<SgxOrderingInput> {
        info!("[Ordering-Only Mode] Preparing {} orders with pre-calculated SimValues for SGX MEV-protected ordering", block_orders.get_all_orders().len());
        
        let orders: Vec<OrderForOrdering> = block_orders.get_all_orders()
            .into_iter()
            .map(|sim_order| {
                let order_id = sim_order.id().to_string();
                OrderForOrdering {
                    id: order_id.clone(),
                    order_type: match &sim_order.order {
                        crate::primitives::Order::Tx(_) => "transaction".to_string(),
                        crate::primitives::Order::Bundle(_) => "bundle".to_string(),
                        crate::primitives::Order::ShareBundle(_) => "share_bundle".to_string(),
                    },
                    coinbase_profit: sim_order.sim_value.coinbase_profit,
                    gas_used: sim_order.sim_value.gas_used,
                    gas_price: sim_order.sim_value.mev_gas_price,
                    order_hash: order_id,
                }
            })
            .collect();

        let input = SgxOrderingInput {
            block_number: ctx.evm_env.block_env.number.to::<u64>(),
            block_timestamp: ctx.attributes.timestamp,
            base_fee: ctx.evm_env.block_env.basefee,
            gas_limit: ctx.evm_env.block_env.gas_limit.to::<u64>(),
            orders,
        };

        info!("[Ordering-Only Mode] Prepared {} orders for SGX ordering - sample values:", input.orders.len());
        for (i, order) in input.orders.iter().take(3).enumerate() {
            debug!("[Ordering-Only Mode] Order #{}: id={}, type={}, profit={}, gas_used={}, mev_gas_price={}", 
                i + 1, order.id, order.order_type, order.coinbase_profit, order.gas_used, order.gas_price);
        }
        Ok(input)
    }

    fn get_sgx_transaction_ordering(&self, input: &SgxOrderingInput) -> Result<SgxOrderingResult> {
        info!("[Ordering-Only Mode] Requesting transaction ordering from SGX");
        
        let input_json = serde_json::to_string(input)?;
        debug!("[Ordering-Only Mode] SGX ordering input size: {} bytes", input_json.len());

        
        info!("[Ordering-Only Mode] Using SGX order_transactions for trusted MEV-protected ordering");
        match self.sgx_builder.order_transactions(&input_json) {
            Ok(result_json) => {
                debug!("[Ordering-Only Mode] SGX order_transactions completed, result size: {} bytes", result_json.len());
                
                let sgx_output: SgxOrderingOutput = serde_json::from_str(&result_json)
                    .map_err(|e| eyre!("Failed to parse SGX ordering output: {}", e))?;
                
                let ordered_transactions: Vec<OrderedTransaction> = sgx_output.ordered_transaction_ids
                    .clone()
                    .into_iter()
                    .map(|tx_id| {
                        let order = input.orders.iter().find(|o| o.order_hash == tx_id || o.id == tx_id);
                        OrderedTransaction {
                            id: order.map(|o| o.id.clone()).unwrap_or(tx_id.clone()),
                            order_type: order.map(|o| o.order_type.clone()).unwrap_or_else(|| "unknown".to_string()),
                            encoded_signed_tx: Bytes::default(), 
                            order_id: order.map(|o| o.id.clone()).unwrap_or(tx_id.clone()),
                            order_hash: order.map(|o| o.order_hash.clone()).unwrap_or(tx_id),
                        }
                    })
                    .collect();
                
                let ordering_result = SgxOrderingResult {
                    ordered_transaction_ids: sgx_output.ordered_transaction_ids,
                    ordered_transactions,
                    sgx_signature: sgx_output.signature.clone(), 
                    block_number: sgx_output.block_number,
                    timestamp: sgx_output.timestamp,
                };
                
                info!("[Ordering-Only Mode] SGX completed MEV-protected ordering: {} transactions sorted", ordering_result.ordered_transactions.len());
                
                let original_order: Vec<String> = input.orders.iter().map(|o| o.id.clone()).collect();
                let sgx_order = &ordering_result.ordered_transaction_ids;
                let order_changed = original_order != *sgx_order;
                info!("[Ordering-Only Mode] Order changed by SGX: {} (original != sorted)", order_changed);
                Ok(ordering_result)
            },
            Err(e) => {
                error!("[Ordering-Only Mode] SGX order_transactions failed: {}", e);
                Err(eyre!("SGX ordering failed: {}", e))
            }
        }
    }

    fn execute_sgx_ordered_transactions<P: provider::StateProviderFactory + Send + Sync + 'static + Clone>(
        &self,
        ordering_result: &SgxOrderingResult,
        original_orders: &ValidatedOrderMap,
        ctx: &BlockBuildingContext,
        provider: &P,
        state_provider: &Arc<dyn StateProvider>,
        name: &str,
    ) -> Result<BiddableUnfinishedBlock> {
        info!("[Ordering-Only Mode] Executing {} SGX-ordered transactions with full Reth infrastructure", 
            ordering_result.ordered_transactions.len());
        
        let latest_provider = provider.latest()?;
        let mut host_helper = block_building_helper::BlockBuildingHelperFromProvider::new(
            Arc::from(latest_provider),
            ctx.clone(),
            None,
            format!("{}-OptionC", name),
            true, 
            CancellationToken::new(),
        )?;

        let mut committed_count = 0;
        
        let order_lookup: std::collections::HashMap<String, Arc<crate::primitives::SimulatedOrder>> = 
            original_orders.get_all_orders()
                .into_iter()
                .map(|sim_order| (sim_order.id().to_string(), sim_order))
                .collect();

        for ordered_tx in &ordering_result.ordered_transactions {
            trace!("[Ordering-Only Mode] Executing ordered transaction: {}", ordered_tx.order_id);
            
            let sim_order = match order_lookup.get(&ordered_tx.order_id) {
                Some(order) => order.clone(),
                None => {
                    warn!("[Ordering-Only Mode] Could not find original order for ID: {}", ordered_tx.order_id);
                    continue;
                }
            };

            match host_helper.commit_order(
                &sim_order,
                &|_sim_value| Ok(()) 
            ) {
                Ok(Ok(_execution_result)) => {
                    debug!("[Ordering-Only Mode] Successfully executed ordered transaction: {}", ordered_tx.order_id);
                    committed_count += 1;
                },
                Ok(Err(e)) => {
                    debug!("[Ordering-Only Mode] Ordered transaction execution failed: {}, error: {}", ordered_tx.order_id, e);
                },
                Err(e) => {
                    error!("[Ordering-Only Mode] Critical error executing ordered transaction {}: {}", ordered_tx.order_id, e);
                }
            }
        }

        info!("[Ordering-Only Mode] Host execution completed. Executed {} out of {} SGX-ordered transactions", 
            committed_count, ordering_result.ordered_transactions.len());

        let unfinished_block = BiddableUnfinishedBlock::new(Box::new(host_helper))?;
        Ok(unfinished_block)
    }

    fn verify_sgx_ordering_signature(&self, ordering_result: &SgxOrderingResult) -> Result<()> {
        info!("[Ordering-Only Mode] Verifying SGX signature on ordering decision");
        
        let _signature_bytes = ordering_result.sgx_signature
            .as_ref()
            .ok_or_else(|| eyre!("No SGX signature available for verification"))?;
        
        let signed_output = block_builder_types::SgxOrderingOutput {
            ordered_transaction_ids: ordering_result.ordered_transaction_ids.clone(),
            block_number: ordering_result.block_number,
            timestamp: ordering_result.timestamp,
            signature: ordering_result.sgx_signature.clone(),
        };
        
        let output_json = serde_json::to_string(&signed_output)
            .map_err(|e| eyre!("Failed to serialize ordering result for verification: {}", e))?;
        
        match self.verifier.verify_ordering_output(&output_json) {
            Ok(_) => {
                info!("[Ordering-Only Mode] SGX ordering signature verified successfully");
                Ok(())
            },
            Err(e) => {
                error!("[Ordering-Only Mode] SGX ordering signature verification failed: {}", e);
                Err(eyre!("SGX ordering signature verification failed: {}", e))
            }
        }
    }
}
