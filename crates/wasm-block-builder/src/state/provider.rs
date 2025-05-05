use crate::interfaces::input::{SerializedAccount, SerializedCode, SerializedStorage};
use alloy_primitives::{Address, B256, U256};
use hashbrown::HashMap;
use std::collections::hash_map::RandomState as StdRandomState;
use revm::{
    primitives::{AccountInfo, Bytecode, HashMap as RevmHashMap},
    Database, DatabaseCommit,
};
use thiserror::Error;

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

impl WasiStateProvider {
    pub fn new(
        accounts: Vec<SerializedAccount>,
        storage: Vec<SerializedStorage>,
        code: Vec<SerializedCode>,
    ) -> Self {
        let accounts_map = accounts
            .into_iter()
            .map(|account| {
                let account_info = AccountInfo {
                    balance: account.balance,
                    nonce: account.nonce,
                    code_hash: account.code_hash,
                    code: None,
                };
                (account.address, account_info)
            })
            .collect();
            
        let storage_map = storage
            .into_iter()
            .map(|storage| ((storage.address, storage.slot), storage.value))
            .collect();
            
        let code_map = code
            .into_iter()
            .map(|code| {
                let bytecode = Bytecode::new_raw(code.bytecode);
                (code.hash, bytecode)
            })
            .collect();
            
        Self {
            accounts: accounts_map,
            storage: storage_map,
            code: code_map,
            block_hashes: HashMap::with_hasher(StdRandomState::new()),
        }
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
}

impl Database for WasiStateProvider {
    type Error = StateError;
    
    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(&address).cloned())
    }
    
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
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
