use crate::{evm, evm::UsedStateTrace, interfaces::{
    input::{BlockBuilderConfig, BlockParams, SerializedTransaction, SerializedAccount, SerializedCode, SerializedStorage},
    output::{SerializedReceipt, SerializedStateDiff, SerializedAccountDiff, SerializedStorageDiff, SerializedCodeDiff, ChunkInfo},
}, sgx_log, state::{
    WasiStateProvider, StateDiffCollector, StateDiffCollectorSettings,
    CompressionLevel, EnhancedStateDiff, diff::AccessType, DiffEncoder
}};
use alloy_primitives::{Address, B256, Bytes, U256};
use hashbrown::{HashMap, HashSet};
use std::collections::hash_map::RandomState as StdRandomState;
use super::BlockBuilderError;
use super::ordering::OrderedTransaction;
use log::{debug, error, info};
use crate::evm::SlotKey;
use uuid::Uuid;

#[derive(Clone)]
pub struct WasiSimulator {
    gas_used: u64,
    
    blob_gas_used: u64,
    
    failed_tx_count: usize,
    
    touched_accounts: HashSet<Address, StdRandomState>,
    
    touched_storage: HashSet<(Address, B256), StdRandomState>,
    
    created_contracts: Vec<Address>,
    
    coinbase_profit: U256,
    
    state_trace: UsedStateTrace,
    
    build_id: Option<String>,
    
    state_diff_chunks: HashMap<String, Vec<EnhancedStateDiff>, StdRandomState>,
}

impl WasiSimulator {
    pub fn new() -> Self {
        Self {
            gas_used: 0,
            blob_gas_used: 0,
            failed_tx_count: 0,
            touched_accounts: HashSet::with_hasher(StdRandomState::new()),
            touched_storage: HashSet::with_hasher(StdRandomState::new()),
            created_contracts: Vec::new(),
            coinbase_profit: U256::ZERO,
            state_trace: UsedStateTrace::default(),
            build_id: None,
            state_diff_chunks: HashMap::with_hasher(StdRandomState::new()),
        }
    }
    
    fn generate_build_id(&mut self) -> String {
        let build_id = format!("build-{}", Uuid::new_v4());
        self.build_id = Some(build_id.clone());
        build_id
    }
    
    fn store_state_diff_chunks(&mut self, build_id: &str, chunks: &[EnhancedStateDiff]) {
        if chunks.is_empty() {
            return;
        }
        
        self.state_diff_chunks.insert(build_id.to_string(), chunks.to_vec());
    }
    
    pub fn get_state_diff_chunk(&self, build_id: &str, chunk_id: u32) -> Option<EnhancedStateDiff> {
        if let Some(chunks) = self.state_diff_chunks.get(build_id) {
            if (chunk_id as usize) < chunks.len() {
                return Some(chunks[chunk_id as usize].clone());
            }
        }
        None
    }
    
    pub fn gas_used(&self) -> u64 {
        self.gas_used
    }
    
    pub fn blob_gas_used(&self) -> u64 {
        self.blob_gas_used
    }
    
    pub fn failed_tx_count(&self) -> usize {
        self.failed_tx_count
    }
    
    pub fn coinbase_profit(&self) -> U256 {
        self.coinbase_profit
    }
    
    pub fn state_trace(&self) -> &UsedStateTrace {
        &self.state_trace
    }
    
    pub fn state_trace_mut(&mut self) -> &mut UsedStateTrace {
        &mut self.state_trace
    }
    
    pub fn simulate_and_build(
        &mut self,
        state: &mut WasiStateProvider,
        ordered_txs: Vec<OrderedTransaction>,
        block_params: &BlockParams,
        config: &BlockBuilderConfig,
    ) -> Result<(Vec<SerializedTransaction>, Vec<SerializedReceipt>), BlockBuilderError> {
        let mut included_txs = Vec::new();
        let mut receipts = Vec::new();
        
        let mut current_gas_used = 0u64;
        let mut current_blob_gas_used = 0u64;
        
        let mut account_nonces: HashMap<Address, u64, StdRandomState> = 
            HashMap::with_hasher(StdRandomState::new());
        
        let mut bundle_included = HashSet::with_hasher(StdRandomState::new());
        let mut bundle_failed = HashSet::with_hasher(StdRandomState::new());
        
        self.gas_used = 0;
        self.blob_gas_used = 0;
        self.failed_tx_count = 0;
        self.touched_accounts.clear();
        self.touched_storage.clear();
        self.created_contracts.clear();
        self.coinbase_profit = U256::ZERO;
        self.state_trace.clear();
        
        for tx in &ordered_txs {
            let address = tx.transaction.from;
            if !account_nonces.contains_key(&address) {
                if let Some(account_info) = state.account_info(address) {
                    account_nonces.insert(address, account_info.nonce);
                } else {
                    account_nonces.insert(address, 0);
                }
            }
        }
        
        for ordered_tx in ordered_txs {
            if current_gas_used + ordered_tx.transaction.gas_limit >= block_params.gas_limit {
                debug!("Block gas limit reached, skipping remaining transactions");
                break;
            }
            
            if ordered_tx.in_bundle {
                if let Some(bundle_hash) = ordered_tx.bundle_hash {
                    if bundle_failed.contains(&bundle_hash) {
                        debug!("Skipping transaction from failed bundle: {:?}", bundle_hash);
                        continue;
                    }
                    
                    if !bundle_included.contains(&bundle_hash) {
                    }
                }
            }
            
            let sender_nonce = *account_nonces
                .get(&ordered_tx.transaction.from)
                .unwrap_or(&0);
                
            let tx_address = ordered_tx.transaction.from;
            let slot0_nonce = if let Some(account_info) = state.account_info(tx_address) {
                let slot_value = state.storage_value(tx_address, U256::ZERO.into());
                if slot_value != B256::ZERO {
                    Some(U256::from_be_bytes(slot_value.0).to::<u64>())
                } else {
                    None
                }
            } else {
                None
            };
            
            let effective_nonce = slot0_nonce.unwrap_or(sender_nonce);
            
            if ordered_tx.transaction.nonce != effective_nonce {
                debug!(
                    "Skipping transaction with incorrect nonce: expected {} (header: {}, slot0: {:?}), got {}",
                    effective_nonce,
                    sender_nonce,
                    slot0_nonce,
                    ordered_tx.transaction.nonce
                );
                continue;
            }
            
            info!("Executing transaction: {:?}", ordered_tx.transaction.hash);
            let result = match evm::execute_transaction_with_trace(
                &ordered_tx.transaction,
                state, 
                block_params,
                &mut self.state_trace
            ) {
                Ok(result) => result,
                Err(err) => {
                    let error_msg = format!("Transaction execution failed: {:?}, tx hash: {:?}, from: {:?}, to: {:?}, nonce: {}, gas: {}, gas_price: {:?}, value: {:?}", 
                           err, 
                           ordered_tx.transaction.hash,
                           ordered_tx.transaction.from,
                           ordered_tx.transaction.to,
                           ordered_tx.transaction.nonce,
                           ordered_tx.transaction.gas_limit,
                           ordered_tx.transaction.gas_price,
                           ordered_tx.transaction.value);
                    log::error!("{}", error_msg);
                    sgx_log(&error_msg);
                    
                    if let Some(account_info) = state.account_info(ordered_tx.transaction.from) {
                        debug!("Sender account state: balance={:?}, nonce={}, code_hash={:?}", 
                               account_info.balance, account_info.nonce, account_info.code_hash);
                    } else {
                        debug!("Sender account not found in state");
                    }
                    
                    if let Some(to) = ordered_tx.transaction.to {
                        if let Some(account_info) = state.account_info(to) {
                            debug!("Recipient account state: balance={:?}, nonce={}, code_hash={:?}", 
                                   account_info.balance, account_info.nonce, account_info.code_hash);
                        } else {
                            debug!("Recipient account not found in state");
                        }
                    }
                    
                    if let Some(bundle_hash) = ordered_tx.bundle_hash {
                        bundle_failed.insert(bundle_hash);
                    }
                    self.failed_tx_count += 1;
                    
                    if !config.discard_txs {
                        break;
                    }
                    continue;
                }
            };
            
            if !result.success {
                debug!("Transaction reverted");
                if ordered_tx.in_bundle {
                    if let Some(bundle_hash) = ordered_tx.bundle_hash {
                        bundle_failed.insert(bundle_hash);
                    }
                }
                self.failed_tx_count += 1;
                
                if !config.discard_txs {
                    break;
                }
                continue;
            }
            
            current_gas_used += result.gas_used;
            current_blob_gas_used += result.blob_gas_used;
            
            account_nonces.insert(ordered_tx.transaction.from, sender_nonce + 1);
            
            if let Some(bundle_hash) = ordered_tx.bundle_hash {
                bundle_included.insert(bundle_hash);
            }
            
            self.coinbase_profit += result.coinbase_profit;
            
            self.touched_accounts.insert(ordered_tx.transaction.from);
            if let Some(to) = ordered_tx.transaction.to {
                self.touched_accounts.insert(to);
            }
            
            let receipt = SerializedReceipt {
                tx_type: ordered_tx.transaction.tx_type as u8,
                success: result.success,
                cumulative_gas_used: current_gas_used,
                logs: Vec::new(),
                logs_bloom: [0u8; 256],
            };
            
            included_txs.push(ordered_tx.transaction);
            receipts.push(receipt);
        }
        
        self.gas_used = current_gas_used;
        self.blob_gas_used = current_blob_gas_used;
        
        Ok((included_txs, receipts))
    }
    
    pub fn calculate_state_diff(
        &mut self, 
        state: &WasiStateProvider,
        include_complete_diff: bool,
        include_proofs: bool,
        compression_level: CompressionLevel,
    ) -> Result<(SerializedStateDiff, Option<EnhancedStateDiff>, Option<ChunkInfo>, Option<String>), BlockBuilderError> {
        let mut basic_state_diff = SerializedStateDiff {
            accounts: Vec::new(),
            storage: Vec::new(),
            code: Vec::new(),
        };
        
        let mut accounts_to_check = HashSet::with_hasher(StdRandomState::new());
        
        for address in self.state_trace.read_balances.keys() {
            accounts_to_check.insert(*address);
        }
        
        for address in self.state_trace.read_nonces.keys() {
            accounts_to_check.insert(*address);
        }
        
        for address in self.state_trace.written_nonces.keys() {
            accounts_to_check.insert(*address);
        }
        
        for slot_key in self.state_trace.read_slots.keys() {
            accounts_to_check.insert(slot_key.address);
        }
        for slot_key in self.state_trace.written_slots.keys() {
            accounts_to_check.insert(slot_key.address);
        }
        
        for address in &self.state_trace.created_contracts {
            accounts_to_check.insert(*address);
        }
        for address in &self.state_trace.destroyed_contracts {
            accounts_to_check.insert(*address);
        }
        
        for address in &self.touched_accounts {
            accounts_to_check.insert(*address);
        }
        
        let mut collector = None;
        if include_complete_diff {
            let settings = StateDiffCollectorSettings {
                include_read_only: true,
                include_proofs,
                compression_level,
                max_diff_size: 10 * 1024 * 1024, 
                max_entries_per_chunk: 5000,      
            };
            collector = Some(StateDiffCollector::new(settings));
        }
        
        for address in accounts_to_check {
            if let Some(account_info) = state.account_info(address) {
                let old_balance = self.state_trace.read_balances.get(&address).cloned();
                let old_nonce = self.state_trace.read_nonces.get(&address).cloned();
                
                let old_nonce = self.state_trace.read_nonces.get(&address).cloned();
                let new_nonce = if account_info.nonce == old_nonce.unwrap_or(0)
                    && self.state_trace
                        .written_slots
                        .get(&SlotKey { address, key: U256::ZERO.into() })
                        .is_some()
                {
                    Some(
                        U256::from_be_bytes(
                            self.state_trace
                                .written_slots[&SlotKey { address, key: U256::ZERO.into() }].0
                        ).to::<u64>()
                    )
                } else if self.state_trace.written_nonces.contains_key(&address) {
                    Some(self.state_trace.written_nonces[&address])
                } else {
                    Some(account_info.nonce)
                };
                
                let account_diff = SerializedAccountDiff {
                    address,
                    old_balance,
                    new_balance: Some(account_info.balance),
                    old_nonce,
                    new_nonce,
                    old_code_hash: None,
                    new_code_hash: Some(account_info.code_hash),
                };
                basic_state_diff.accounts.push(account_diff);
                
                if let Some(bytecode) = account_info.code.clone() {
                    if !bytecode.is_empty() {
                        if !basic_state_diff.code.iter().any(|c| c.hash == account_info.code_hash) {
                            basic_state_diff.code.push(SerializedCodeDiff {
                                hash: account_info.code_hash,
                                bytecode: bytecode.bytecode().clone(),
                            });
                            
                            if let Some(collector) = &mut collector {
                                collector.record_code_change(
                                    account_info.code_hash, 
                                    bytecode.bytecode().clone(),
                                    self.state_trace.created_contracts.contains(&address)
                                );
                            }
                        }
                    }
                }
                
                if let Some(collector) = &mut collector {
                    if self.state_trace.created_contracts.contains(&address) {
                        collector.record_account_create(&address, &revm::primitives::CreateScheme::Create, account_info);
                    } else if self.state_trace.read_balances.contains_key(&address) || self.state_trace.read_nonces.contains_key(&address) {
                        collector.record_account_update(&address, account_info);
                    }
                }
            } else if self.state_trace.destroyed_contracts.contains(&address) {
                if let Some(collector) = &mut collector {
                    collector.record_account_delete(&address);
                }
            }
        }
        
        for (slot_key, old_value) in &self.state_trace.read_slots {
            if let Some(new_value) = self.state_trace.written_slots.get(slot_key) {
                if old_value != new_value {
                    basic_state_diff.storage.push(SerializedStorageDiff {
                        address: slot_key.address,
                        slot: slot_key.key,
                        old_value: *old_value,
                        new_value: *new_value,
                    });
                    
                    if let Some(collector) = &mut collector {
                        collector.record_storage_update(
                            &slot_key.address,
                            slot_key.key,
                            *new_value,
                            *old_value
                        );
                    }
                } else {
                    if let Some(collector) = &mut collector {
                        collector.record_storage_read(
                            &slot_key.address,
                            slot_key.key,
                            *old_value
                        );
                    }
                }
            }
        }
        
        for (slot_key, new_value) in &self.state_trace.written_slots {
            if !self.state_trace.read_slots.contains_key(slot_key) {
                basic_state_diff.storage.push(SerializedStorageDiff {
                    address: slot_key.address,
                    slot: slot_key.key,
                    old_value: B256::ZERO,
                    new_value: *new_value,
                });
                
                if let Some(collector) = &mut collector {
                    collector.record_storage_update(
                        &slot_key.address,
                        slot_key.key,
                        *new_value,
                        B256::ZERO
                    );
                }
            }
        }
        
        for (address, slot) in &self.touched_storage {
            let slot_key = SlotKey {
                address: *address,
                key: *slot,
            };
            
            if self.state_trace.written_slots.contains_key(&slot_key) {
                continue;
            }
            
            let value = state.storage_value(*address, *slot);
            
            basic_state_diff.storage.push(SerializedStorageDiff {
                address: *address,
                slot: *slot,
                old_value: B256::ZERO,
                new_value: value,
            });
            
            if let Some(collector) = &mut collector {
                collector.record_storage_read(
                    address,
                    *slot,
                    value
                );
            }
        }
        
        let mut enhanced_diff = None;
        let mut chunk_info = None;
        let mut build_id = None;
        
        if let Some(collector) = &collector {
            let state_root = B256::ZERO;
            let block_number = 0; 
            
            let diff = collector.generate_diff(state, state_root, block_number);
            
            if diff.estimated_size() > collector.settings.max_diff_size {
                debug!("Enhanced state diff is too large ({} bytes), splitting into chunks", 
                    diff.estimated_size());
                
                let chunks = diff.split_into_chunks(collector.settings.max_diff_size);
                debug!("Split enhanced state diff into {} chunks", chunks.len());
                
                if !chunks.is_empty() {
                    let _build_id = self.generate_build_id();
                    build_id = self.build_id.clone();
                    
                    if let Some(build_id_str) = &build_id {
                        self.store_state_diff_chunks(build_id_str, &chunks[1..]);
                    }
                    
                    enhanced_diff = Some(chunks[0].clone());
                    
                    chunk_info = Some(ChunkInfo {
                        chunk_id: 0,
                        total_chunks: chunks.len() as u32,
                        remaining_chunks_available: chunks.len() > 1,
                    });
                }
            } else {
                enhanced_diff = Some(diff);
            }
        }
        
        Ok((basic_state_diff, enhanced_diff, chunk_info, build_id))
    }
}