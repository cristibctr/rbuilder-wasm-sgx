use crate::interfaces::input::{SerializedAccount, SerializedCode, SerializedStorage};
use alloy_primitives::{Address, B256, U256, Bytes};
use hashbrown::HashMap;
use std::collections::hash_map::RandomState as StdRandomState;
use revm::{
    primitives::{AccountInfo, Bytecode, HashMap as RevmHashMap},
    Database, DatabaseCommit,
};
use thiserror::Error;
use std::str::FromStr;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum StateError {
    #[error("Account not found: {0}")]
    AccountNotFound(Address),
    
    #[error("Code not found for hash: {0}")]
    CodeNotFound(B256),
    
    #[error("Storage slot not found: {0}:{1}")]
    StorageNotFound(Address, B256),

    #[error("Block hash not found for number: {0}")]
    BlockHashNotFound(u64),
}


#[derive(Clone)]
pub struct WasiStateProvider {
    accounts: HashMap<Address, AccountInfo, StdRandomState>,
    
    storage: HashMap<(Address, B256), B256, StdRandomState>,
    
    code: HashMap<B256, Bytecode, StdRandomState>,
    
    block_hashes: HashMap<u64, B256, StdRandomState>,
}

use alloy_rlp;
use revm::primitives::KECCAK_EMPTY;
use crate::sgx_log;
use super::diff::MerkleProof;

impl WasiStateProvider {
    pub fn new(
        accounts: Vec<SerializedAccount>,
        storage: Vec<SerializedStorage>,
        code: Vec<SerializedCode>,
    ) -> Self {
        let error_msg = format!("Initializing WasiStateProvider with {} accounts, {} storage entries, {} code entries",
                                accounts.len(), storage.len(), code.len());
        log::debug!("{}", error_msg);
        sgx_log(&error_msg);
        
        let accounts_map = accounts
            .into_iter()
            .map(|account| {
                let account_info = AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.code_hash,
                    code: None,
                };
                let error_msg = format!("Added account: address={:?}, balance={:?}, nonce={}, code_hash={:?}",
                                        account.address, account_info.balance, account_info.nonce, account_info.code_hash);
                log::debug!("{}", error_msg);
                sgx_log(&error_msg);
                (account.address, account_info)
            })
            .collect();
            
        let storage_map = storage
            .into_iter()
            .map(|storage| {
                let error_msg = format!("Added storage: address={:?}, slot={:?}, value={:?}",
                                        storage.address, storage.slot, storage.value);
                log::debug!("{}", error_msg);
                sgx_log(&error_msg);
                ((storage.address, storage.slot), storage.value)
            })
            .collect();
            
        let code_map = code
            .into_iter()
            .map(|code| {
                let bytecode = Bytecode::new_raw(code.bytecode);
                let error_msg = format!("Added code: hash={:?}, bytecode_len={}",
                                        code.hash, bytecode.bytecode().len());
                log::debug!("{}", error_msg);
                sgx_log(&error_msg);

                (code.hash, bytecode)
            })
            .collect();
            
        let provider = Self {
            accounts: accounts_map,
            storage: storage_map,
            code: code_map,
            block_hashes: HashMap::with_hasher(StdRandomState::new()),
        };
        
        let error_msg = format!("WasiStateProvider initialized successfully");
        log::debug!("{}", error_msg);
        sgx_log(&error_msg);

        provider
    }
    
    pub fn account_info(&self, address: Address) -> Option<AccountInfo> {
        self.accounts.get(&address).cloned()
    }
    
    pub fn storage_value(&self, address: Address, slot: B256) -> B256 {
        self.storage
            .get(&(address, slot))
            .cloned()
            .unwrap_or(B256::ZERO)
    }
    
    pub fn code_by_hash(&self, code_hash: B256) -> Option<Bytecode> {
        self.code.get(&code_hash).cloned()
    }
    
    pub fn set_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }
    
    pub fn block_hash_by_number(&self, number: u64) -> Option<B256> {
        self.block_hashes.get(&number).cloned()
    }
    
    pub fn get_storage_root(&self, address: &Address) -> Option<B256> {
        if let Some(account) = self.accounts.get(address) {
            let slots: Vec<(B256, B256)> = self.storage
                .iter()
                .filter_map(|((addr, slot), value)| {
                    if addr == address {
                        Some((*slot, *value))
                    } else {
                        None
                    }
                })
                .collect();
                
            if slots.is_empty() {
                Some(KECCAK_EMPTY)
            } else {
                let mut sorted_slots = slots.clone();
                sorted_slots.sort_by(|a, b| a.0.cmp(&b.0));
                
                let mut serialized = Vec::new();
                for (slot, value) in sorted_slots {
                    serialized.extend_from_slice(slot.as_slice());
                    serialized.extend_from_slice(value.as_slice());
                }
                
                Some(alloy_primitives::keccak256(serialized))
            }
        } else {
            None
        }
    }
    
    pub fn get_account_proof(&self, address: &Address) -> Option<MerkleProof> {
        if self.accounts.get(address).is_none() {
            return None;
        }
        
        
        let account_key_hash = alloy_primitives::keccak256(address.as_slice());
        
        let account_info = self.accounts.get(address)?;
        
        let storage_root = self.get_storage_root(address).unwrap_or_else(|| {
            B256::from_str("0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421").unwrap_or_default()
        });
        
        let account_value = serde_json::to_vec(&serde_json::json!({
            "nonce": account_info.nonce.to_string(),
            "balance": account_info.balance.to_string(),
            "storageRoot": format!("{:?}", storage_root),
            "codeHash": format!("{:?}", account_info.code_hash),
        })).unwrap_or_default();
        
        let mut proof_nodes = Vec::with_capacity(3);
        
        let root_branch_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "branch",
            "path": account_key_hash[0].to_string(),
            "children": {
                (account_key_hash[0] % 16).to_string(): "<next_node_hash>"
            }
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(root_branch_serialized));
        
        let extension_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "extension",
            "path": format!("{:?}", &account_key_hash[1..5]),
            "next": "<leaf_node_hash>"
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(extension_serialized));
        
        let leaf_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "leaf",
            "path": format!("{:?}", &account_key_hash[5..]),
            "value": {
                "nonce": account_info.nonce,
                "balance": account_info.balance.to_string(),
                "storageRoot": format!("{:?}", storage_root),
                "codeHash": format!("{:?}", account_info.code_hash)
            }
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(leaf_serialized));
        
        Some(MerkleProof {
            proof: proof_nodes,
            key: Bytes::from(account_key_hash.as_slice().to_vec()),
            value: Bytes::from(account_value),
        })
    }
    
    pub fn get_storage_proof(&self, address: &Address, slot: &B256) -> Option<MerkleProof> {
        if !self.storage.contains_key(&(*address, *slot)) {
            return None;
        }
        
        
        let storage_key_hash = alloy_primitives::keccak256(slot.as_slice());
        
        let value = self.storage.get(&(*address, *slot)).unwrap_or(&B256::ZERO);
        
        let storage_value = serde_json::to_vec(&serde_json::json!({
            "value": format!("{:?}", value)
        })).unwrap_or_default();
        
        let mut proof_nodes = Vec::with_capacity(3);
        
        let root_branch_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "branch",
            "path": storage_key_hash[0].to_string(),
            "children": {
                (storage_key_hash[0] % 16).to_string(): "<next_node_hash>"
            }
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(root_branch_serialized));
        
        let mid_branch_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "branch",
            "path": storage_key_hash[1].to_string(),
            "children": {
                (storage_key_hash[1] % 16).to_string(): "<leaf_node_hash>"
            }
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(mid_branch_serialized));
        
        let leaf_serialized = serde_json::to_vec(&serde_json::json!({
            "type": "leaf",
            "path": format!("{:?}", &storage_key_hash[2..]),
            "value": format!("{:?}", value)
        })).unwrap_or_default();
        proof_nodes.push(Bytes::from(leaf_serialized));
        
        Some(MerkleProof {
            proof: proof_nodes,
            key: Bytes::from(storage_key_hash.as_slice().to_vec()),
            value: Bytes::from(storage_value),
        })
    }
}

impl Database for WasiStateProvider {
    type Error = StateError;
    
    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(&address).cloned())
    }
    
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY || code_hash == B256::ZERO {
            return Ok(Bytecode::new());
        }
        
        self.code
            .get(&code_hash)
            .cloned()
            .ok_or(StateError::CodeNotFound(code_hash))
    }
    
    fn storage(&mut self, address: Address, slot_u256: U256) -> Result<U256, Self::Error> {
        let slot_b256 = B256::from(slot_u256);
        let value = self.storage
            .get(&(address, slot_b256))
            .copied()
            .unwrap_or(B256::ZERO);
        Ok(U256::from_be_bytes(value.0))
    }
    
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.block_hashes
            .get(&number)
            .copied()
            .ok_or(StateError::BlockHashNotFound(number))
    }
}

impl DatabaseCommit for WasiStateProvider {
    fn commit(&mut self, changes: RevmHashMap<Address, revm::primitives::Account>) {
        for (address, account) in changes {
            if account.is_selfdestructed() {
                self.accounts.remove(&address);
            } else {
                let info = &account.info;
                let mut current_info = self.accounts.entry(address).or_insert_with(|| info.clone());
                current_info.balance = info.balance;
                current_info.nonce = info.nonce;
                if current_info.code_hash != info.code_hash {
                    current_info.code_hash = info.code_hash;
                    if let Some(bytecode) = &info.code {
                        if !bytecode.is_empty() && info.code_hash != revm::primitives::KECCAK_EMPTY {
                            self.code.insert(info.code_hash, bytecode.clone());
                        }
                    } else {
                    }
                } else if let Some(bytecode) = &info.code {
                     if !bytecode.is_empty() && info.code_hash != revm::primitives::KECCAK_EMPTY {
                         self.code.insert(info.code_hash, bytecode.clone());
                     }
                }

                for (slot_u256, value) in &account.storage {
                    let slot_b256 = B256::from(*slot_u256);
                    let current_value = value.present_value();
                    if current_value == U256::ZERO {
                        self.storage.remove(&(address, slot_b256));
                    } else {
                        self.storage.insert((address, slot_b256), B256::from(current_value));
                    }
                }
            }
        }
    }
}
