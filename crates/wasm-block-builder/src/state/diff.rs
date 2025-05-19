use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use hashbrown::HashMap as HashbrownMap;
use std::collections::hash_map::RandomState as StdRandomState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountValueStatus {
    Unchanged,
    
    Modified(String),
    
    Deleted,
    
    Created,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    Created,
    
    Modified,
    
    Deleted,
    
    Touched,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SlotStatus {
    Created,
    
    Modified,
    
    Deleted,
    
    Accessed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessType {
    Read,
    
    Write,
    
    Delete,
    
    Create,
}

impl AccessType {
    pub fn is_write(&self) -> bool {
        matches!(self, Self::Write | Self::Delete | Self::Create)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotChange {
    pub slot: B256,
    
    pub new_value: B256,
    
    pub old_value: Option<B256>,
    
    pub status: SlotStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedStorageDiff {
    pub address: Address,
    
    pub slot_changes: Vec<SlotChange>,
    
    pub read_only_slots: Option<Vec<B256>>,
    
    pub proofs: Option<HashMap<B256, MerkleProof, StdRandomState>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    pub proof: Vec<Bytes>,
    
    pub key: Bytes,
    
    pub value: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedAccountDiff {
    pub address: Address,
    
    pub balance: Option<(U256, AccountValueStatus)>,
    
    pub nonce: Option<(u64, AccountValueStatus)>,
    
    pub code_hash: Option<(B256, AccountValueStatus)>,
    
    pub status: AccountStatus,
    
    pub storage_root: Option<B256>,
    
    pub proof: Option<MerkleProof>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedCodeDiff {
    pub hash: B256,
    
    pub bytecode: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedStateDiff {
    pub accounts: Vec<EnhancedAccountDiff>,
    
    pub storage: Vec<EnhancedStorageDiff>,
    
    pub code: Vec<EnhancedCodeDiff>,
    
    pub deleted_accounts: Vec<Address>,
    
    pub deleted_storage: Vec<(Address, B256)>,
    
    pub state_root: B256,
    
    pub generated_at_block: u64,
    
    pub chunk_id: Option<u32>,
    
    pub total_chunks: Option<u32>,
}

impl EnhancedStateDiff {
    pub fn new(state_root: B256, block_number: u64) -> Self {
        Self {
            accounts: Vec::new(),
            storage: Vec::new(),
            code: Vec::new(),
            deleted_accounts: Vec::new(),
            deleted_storage: Vec::new(),
            state_root,
            generated_at_block: block_number,
            chunk_id: None, 
            total_chunks: None,
        }
    }
    
    pub fn estimated_size(&self) -> usize {
        let mut size = std::mem::size_of::<EnhancedStateDiff>();
        
        for account in &self.accounts {
            size += std::mem::size_of::<EnhancedAccountDiff>();
            
            if account.balance.is_some() {
                size += std::mem::size_of::<U256>();
            }
            if account.nonce.is_some() {
                size += std::mem::size_of::<u64>();
            }
            if account.code_hash.is_some() {
                size += std::mem::size_of::<B256>();
            }
            if account.storage_root.is_some() {
                size += std::mem::size_of::<B256>();
            }
        }
        
        for storage in &self.storage {
            size += std::mem::size_of::<EnhancedStorageDiff>();
            
            size += storage.slot_changes.len() * std::mem::size_of::<SlotChange>();
            
            if let Some(read_only_slots) = &storage.read_only_slots {
                size += read_only_slots.len() * std::mem::size_of::<B256>();
            }
        }
        
        for code in &self.code {
            size += std::mem::size_of::<EnhancedCodeDiff>();
            size += code.bytecode.len();
        }
        
        size += self.deleted_accounts.len() * std::mem::size_of::<Address>();
        size += self.deleted_storage.len() * (std::mem::size_of::<Address>() + std::mem::size_of::<B256>());
        
        size
    }
    
    pub fn is_mostly_accounts(&self) -> bool {
        let account_size = self.accounts.len() * std::mem::size_of::<EnhancedAccountDiff>();
        let storage_size = self.storage.iter()
            .map(|s| std::mem::size_of::<EnhancedStorageDiff>() + s.slot_changes.len() * std::mem::size_of::<SlotChange>())
            .sum::<usize>();
        
        account_size > storage_size
    }
    
    pub fn is_mostly_storage(&self) -> bool {
        let account_size = self.accounts.len() * std::mem::size_of::<EnhancedAccountDiff>();
        let storage_size = self.storage.iter()
            .map(|s| std::mem::size_of::<EnhancedStorageDiff>() + s.slot_changes.len() * std::mem::size_of::<SlotChange>())
            .sum::<usize>();
        
        storage_size > account_size
    }
    
    pub fn split_into_chunks(self, max_chunk_size: usize) -> Vec<EnhancedStateDiff> {
        if self.estimated_size() <= max_chunk_size {
            return vec![self];
        }
        
        let estimated_size = self.estimated_size();
        let mut chunk_count = (estimated_size + max_chunk_size - 1) / max_chunk_size;
        chunk_count = chunk_count.max(1);
        
        let mut chunks = Vec::with_capacity(chunk_count);
        
        for chunk_id in 0..chunk_count {
            let mut chunk = EnhancedStateDiff::new(self.state_root, self.generated_at_block);
            chunk.chunk_id = Some(chunk_id as u32);
            chunk.total_chunks = Some(chunk_count as u32);
            chunks.push(chunk);
        }
        
        let mut account_map: HashMap<Address, EnhancedAccountDiff> = HashMap::new();
        for account in self.accounts {
            account_map.insert(account.address, account);
        }
        
        let mut address_to_storage: HashMap<Address, Vec<SlotChange>> = HashMap::new();
        for storage_diff in self.storage {
            let slots = address_to_storage.entry(storage_diff.address).or_default();
            slots.extend(storage_diff.slot_changes);
        }
        
        let code_per_chunk = (self.code.len() + chunk_count - 1) / chunk_count;
        
        let mut current_chunk = 0;
        
        for (i, code) in self.code.into_iter().enumerate() {
            let chunk_idx = i / code_per_chunk;
            chunks[chunk_idx].code.push(code);
        }
        
        for (address, account) in account_map {
            let mut chunk = &mut chunks[current_chunk];
            
            chunk.accounts.push(account);
            
            if let Some(slots) = address_to_storage.remove(&address) {
                if !slots.is_empty() {
                    chunk.storage.push(EnhancedStorageDiff {
                        address,
                        slot_changes: slots,
                        read_only_slots: None,
                        proofs: None,
                    });
                }
            }
            
            current_chunk = (current_chunk + 1) % chunk_count;
        }
        
        for (address, slots) in address_to_storage {
            if !slots.is_empty() {
                let chunk = &mut chunks[current_chunk];
                chunk.storage.push(EnhancedStorageDiff {
                    address,
                    slot_changes: slots,
                    read_only_slots: None,
                    proofs: None,
                });
                
                current_chunk = (current_chunk + 1) % chunk_count;
            }
        }
        
        let deleted_accounts_per_chunk = (self.deleted_accounts.len() + chunk_count - 1) / chunk_count;
        let deleted_storage_per_chunk = (self.deleted_storage.len() + chunk_count - 1) / chunk_count;
        
        for (i, address) in self.deleted_accounts.into_iter().enumerate() {
            let chunk_idx = i / deleted_accounts_per_chunk;
            chunks[chunk_idx].deleted_accounts.push(address);
        }
        
        for (i, (address, slot)) in self.deleted_storage.into_iter().enumerate() {
            let chunk_idx = i / deleted_storage_per_chunk;
            chunks[chunk_idx].deleted_storage.push((address, slot));
        }
        
        chunks
    }
}