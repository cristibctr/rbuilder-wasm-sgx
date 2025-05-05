use alloy_primitives::{Address, B256, U256};
use hashbrown::HashMap;
use std::collections::hash_map::RandomState as StdRandomState;
use revm::{
    primitives::{AccountInfo, Bytecode, Account, HashMap as RevmHashMap},
    Database, DatabaseCommit
};
use std::cell::RefCell;
use std::rc::Rc;

use super::provider::{StateError, WasiStateProvider};

pub struct StateCache {
    provider: Rc<RefCell<WasiStateProvider>>,
    
    accounts: HashMap<Address, Option<AccountInfo>, StdRandomState>,
    
    storage: HashMap<(Address, B256), B256, StdRandomState>,
    
    code: HashMap<B256, Bytecode, StdRandomState>,
    
    block_hashes: HashMap<u64, B256, StdRandomState>,
    
    pub stats: CacheStats,
}

#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    pub account_hits: usize,
    
    pub account_misses: usize,
    
    pub storage_hits: usize,
    
    pub storage_misses: usize,
    
    pub code_hits: usize,
    
    pub code_misses: usize,
    
    pub block_hash_hits: usize,
    
    pub block_hash_misses: usize,
}

impl StateCache {
    pub fn new(provider: Rc<RefCell<WasiStateProvider>>) -> Self {
        Self {
            provider,
            accounts: HashMap::with_hasher(StdRandomState::new()),
            storage: HashMap::with_hasher(StdRandomState::new()),
            code: HashMap::with_hasher(StdRandomState::new()),
            block_hashes: HashMap::with_hasher(StdRandomState::new()),
            stats: CacheStats::default(),
        }
    }
    
    pub fn clear(&mut self) {
        self.accounts.clear();
        self.storage.clear();
        self.code.clear();
        self.block_hashes.clear();
    }
    
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }
}

impl Database for StateCache {
    type Error = StateError;
    
    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(account_option) = self.accounts.get(&address) {
            self.stats.account_hits += 1;
            return Ok(account_option.clone());
        }
        
        self.stats.account_misses += 1;
        let account_option = self.provider.borrow_mut().basic(address)?;
        self.accounts.insert(address, account_option.clone());
        Ok(account_option)
    }
    
    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(code) = self.code.get(&code_hash) {
            self.stats.code_hits += 1;
            return Ok(code.clone());
        }
        
        self.stats.code_misses += 1;

        let result = self.provider.borrow_mut().code_by_hash(code_hash);
        if let Some(bytecode) = result {
            self.code.insert(code_hash, bytecode.clone());
            Ok(bytecode)
        } else {
            Err(StateError::CodeNotFound(code_hash))
        }
    }
    
    fn storage(&mut self, address: Address, slot_u256: U256) -> Result<U256, Self::Error> {
        let slot_b256 = B256::from(slot_u256);
        if let Some(value_b256) = self.storage.get(&(address, slot_b256)) {
            self.stats.storage_hits += 1;
            return Ok(U256::from_be_bytes(value_b256.0));
        }
        
        self.stats.storage_misses += 1;
        let value_u256 = self.provider.borrow_mut().storage(address, slot_u256)?;
        self.storage.insert((address, slot_b256), B256::from(value_u256));
        Ok(value_u256)
    }
    
    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        if let Some(hash) = self.block_hashes.get(&number) {
            self.stats.block_hash_hits += 1;
            return Ok(*hash);
        }
        
        self.stats.block_hash_misses += 1;
        let hash = self.provider.borrow_mut().block_hash(number)?;
        self.block_hashes.insert(number, hash);
        Ok(hash)
    }
}

impl DatabaseCommit for StateCache {
    fn commit(&mut self, changes: RevmHashMap<Address, Account>) {
        for (address, account) in changes.iter() {
            if account.is_selfdestructed() {
                self.accounts.insert(*address, None);
                self.storage.retain(|(addr, _), _| *addr != *address);
            } else {
                let info = &account.info;
                self.accounts.insert(*address, Some(info.clone()));
                if let Some(bytecode) = &info.code {
                    if !bytecode.is_empty() && info.code_hash != revm::primitives::KECCAK_EMPTY {
                        self.code.insert(info.code_hash, bytecode.clone());
                    }
                }
                for (slot_u256, value) in &account.storage {
                    let slot_b256 = B256::from(*slot_u256);
                    let current_value = value.present_value();
                    if current_value == U256::ZERO {
                        self.storage.remove(&(*address, slot_b256));
                    } else {
                        self.storage.insert((*address, slot_b256), B256::from(current_value));
                    }
                }
            }
            if !account.storage.is_empty() {
                for (slot_u256, value) in &account.storage {
                   let slot_b256 = B256::from(*slot_u256);
                   let current_value = value.present_value();
                   if current_value == U256::ZERO {
                       self.storage.remove(&(*address, slot_b256));
                   } else {
                       self.storage.insert((*address, slot_b256), B256::from(current_value));
                   }
                }
            }
        }
        
        self.provider.borrow_mut().commit(changes);
    }
}
