use alloy_primitives::{Address, B256, U256, keccak256};
use alloy_rlp::{BufMut, Encodable};
use alloy_trie::{HashBuilder, Nibbles, TrieAccount};
use revm::primitives::KECCAK_EMPTY;
use crate::interfaces::output::{SerializedAccountDiff, SerializedStateDiff};
use crate::state::WasiStateProvider;
use thiserror::Error;
use crate::sgx_log;

#[derive(Error, Debug)]
pub enum StateRootError {
    #[error("State root calculation failed: {0}")]
    Calculation(String),
    
    #[error("Invalid state format: {0}")]
    InvalidState(String),
    
    #[error("RLP encoding error: {0}")]
    RlpError(String),
    
    #[error("State access error: {0}")]
    StateAccessError(String),
    
    #[error("Missing state element: {0}")]
    MissingElement(String),
}


fn calculate_storage_root(
    address: Address,
    changes: &SerializedStateDiff,
    state: &WasiStateProvider,
) -> Result<B256, StateRootError> {
    let mut final_storage: std::collections::BTreeMap<B256, B256> = std::collections::BTreeMap::new();
    
    for (slot, value) in state.get_storage_for_address(address) {
        if value != B256::ZERO {
            final_storage.insert(slot, value);
        }
    }
    
    for storage_diff in changes.storage.iter() {
        if storage_diff.address == address {
            let slot = storage_diff.slot;
            let value = storage_diff.new_value;
            
            if value == B256::ZERO {
                final_storage.remove(&slot);
            } else {
                final_storage.insert(slot, value);
            }
        }
    }
    
    if final_storage.is_empty() {
        return Ok(alloy_trie::EMPTY_ROOT_HASH);
    }
    
    let mut hash_builder = HashBuilder::default();
    
    let mut sorted_entries: Vec<_> = final_storage.into_iter().collect();
    sorted_entries.sort_by_key(|(slot, _)| keccak256(slot.as_slice()));
    
    for (slot, value) in sorted_entries {
        let hashed_slot = keccak256(slot.as_slice());
        let nibbles = Nibbles::unpack(hashed_slot);
        
        let encoded_value = alloy_rlp::encode_fixed_size(&value);
        hash_builder.add_leaf(nibbles, &encoded_value);
    }
    
    Ok(hash_builder.root())
}

pub fn calculate_state_root(
    changes: &SerializedStateDiff, 
    state: &WasiStateProvider
) -> Result<B256, StateRootError> {
    let error_msg = "Calculating state root using Reth-compatible algorithm";
    log::info!("{}", error_msg);
    sgx_log(&error_msg);

    let mut hash_builder = HashBuilder::default();
    
    let mut all_addresses: Vec<Address> = Vec::new();
    
    for acc_diff in &changes.accounts {
        if !all_addresses.contains(&acc_diff.address) {
            all_addresses.push(acc_diff.address);
        }
    }
    
    for storage_diff in &changes.storage {
        if !all_addresses.contains(&storage_diff.address) {
            all_addresses.push(storage_diff.address);
        }
    }
    
    for storage_address in state.get_all_storage_addresses() {
        if !all_addresses.contains(&storage_address) {
            all_addresses.push(storage_address);
        }
    }
    
    let mut account_entries = all_addresses;
    
    let debug_msg = format!("Total addresses to process: {}", account_entries.len());
    log::info!("{}", debug_msg);
    sgx_log(&debug_msg);
    
    let debug_msg = format!("Addresses from account changes: {}", changes.accounts.len());
    log::info!("{}", debug_msg);
    sgx_log(&debug_msg);
    
    let debug_msg = format!("Unique addresses from storage changes: {}", 
        changes.storage.iter().map(|s| s.address).collect::<std::collections::HashSet<_>>().len());
    log::info!("{}", debug_msg);
    sgx_log(&debug_msg);
    
    let debug_msg = format!("Addresses with storage in state: {}", state.get_all_storage_addresses().len());
    log::info!("{}", debug_msg);
    sgx_log(&debug_msg);
    
    account_entries.sort_by_key(|address| keccak256(address.as_slice()));
    
    for address in account_entries {
        let account_diff = changes.accounts.iter()
            .find(|acc_diff| acc_diff.address == address);
        let account_balance = account_diff
            .and_then(|diff| diff.new_balance)
            .unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.balance)
                    .unwrap_or(U256::ZERO)
            });
        
        let account_nonce = account_diff
            .and_then(|diff| diff.new_nonce)
            .unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.nonce)
                    .unwrap_or(0)
            });
        
        let account_code_hash = account_diff
            .and_then(|diff| diff.new_code_hash)
            .unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.code_hash)
                    .unwrap_or(KECCAK_EMPTY)
            });
        
        let storage_root = calculate_storage_root(address, changes, state)?;
        
        let trie_account = TrieAccount {
            nonce: account_nonce,
            balance: account_balance,
            storage_root,
            code_hash: account_code_hash,
        };
        
        let mut account_rlp = Vec::new();
        trie_account.encode(&mut account_rlp);
        
        let hashed_address = keccak256(address.as_slice());
        let nibbles = Nibbles::unpack(hashed_address);
        hash_builder.add_leaf(nibbles, &account_rlp);
        
        let debug_msg = format!(
            "Added account to trie: addr={:?}, nonce={}, balance={}, storage_root={:?}, code_hash={:?}",
            address, account_nonce, account_balance, storage_root, account_code_hash
        );
        log::debug!("{}", debug_msg);
        sgx_log(&debug_msg);
    }
    
    let root = hash_builder.root();
    
    let success_msg = format!("Calculated state root using Reth algorithm: {:?}", root);
    log::info!("{}", success_msg);
    sgx_log(&success_msg);
    
    Ok(root)
}