use crate::{
    evm,
    evm::UsedStateTrace,
    interfaces::{
        input::{BlockBuilderConfig, BlockParams, SerializedTransaction, SerializedAccount, SerializedCode, SerializedStorage},
        output::{SerializedReceipt, SerializedStateDiff, SerializedAccountDiff, SerializedStorageDiff, SerializedCodeDiff},
    },
    state::WasiStateProvider,
};
use alloy_primitives::{Address, B256, Bytes, U256};
use hashbrown::{HashMap, HashSet};
use std::collections::hash_map::RandomState as StdRandomState;
use super::BlockBuilderError;
use super::ordering::OrderedTransaction;
use log::{debug, error, info};
use crate::evm::SlotKey;

pub struct WasiSimulator {
    gas_used: u64,
    
    blob_gas_used: u64,
    
    failed_tx_count: usize,
    
    touched_accounts: HashSet<Address, StdRandomState>,
    
    touched_storage: HashSet<(Address, B256), StdRandomState>,
    
    created_contracts: Vec<Address>,
    
    coinbase_profit: U256,
    
    state_trace: UsedStateTrace,
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
        }
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
                
            if ordered_tx.transaction.nonce != sender_nonce {
                debug!(
                    "Skipping transaction with incorrect nonce: expected {}, got {}",
                    sender_nonce,
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
                    error!("Transaction execution failed: {:?}", err);
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
    
    pub fn calculate_state_diff(&self, state: &WasiStateProvider) -> Result<SerializedStateDiff, BlockBuilderError> {
        let mut state_diff = SerializedStateDiff {
            accounts: Vec::new(),
            storage: Vec::new(),
            code: Vec::new(),
        };
        
        let mut accounts_to_check = HashSet::with_hasher(StdRandomState::new());
        
        for address in self.state_trace.read_balances.keys() {
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
        
        for address in accounts_to_check {
            if let Some(account_info) = state.account_info(address) {
                let old_balance = self.state_trace.read_balances.get(&address).cloned();
                
                let account_diff = SerializedAccountDiff {
                    address,
                    old_balance,
                    new_balance: Some(account_info.balance),
                    old_nonce: None,
                    new_nonce: Some(account_info.nonce),
                    old_code_hash: None,
                    new_code_hash: Some(account_info.code_hash),
                };
                state_diff.accounts.push(account_diff);
                
                if let Some(bytecode) = account_info.code {
                    if !bytecode.is_empty() {
                        if !state_diff.code.iter().any(|c| c.hash == account_info.code_hash) {
                            state_diff.code.push(SerializedCodeDiff {
                                hash: account_info.code_hash,
                                bytecode: bytecode.bytecode().clone(),
                            });
                        }
                    }
                }
            }
        }
        
        for (slot_key, old_value) in &self.state_trace.read_slots {
            if let Some(new_value) = self.state_trace.written_slots.get(slot_key) {
                if old_value != new_value {
                    state_diff.storage.push(SerializedStorageDiff {
                        address: slot_key.address,
                        slot: slot_key.key,
                        old_value: *old_value,
                        new_value: *new_value,
                    });
                }
            }
        }
        
        for (slot_key, new_value) in &self.state_trace.written_slots {
            if !self.state_trace.read_slots.contains_key(slot_key) {
                state_diff.storage.push(SerializedStorageDiff {
                    address: slot_key.address,
                    slot: slot_key.key,
                    old_value: B256::ZERO,
                    new_value: *new_value,
                });
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
            state_diff.storage.push(SerializedStorageDiff {
                address: *address,
                slot: *slot,
                old_value: B256::ZERO,
                new_value: value,
            });
        }
        
        Ok(state_diff)
    }
}