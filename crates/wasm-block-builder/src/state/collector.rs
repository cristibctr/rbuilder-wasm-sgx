use alloy_primitives::{Address, B256, U256};
use hashbrown::{HashMap, HashSet};
use revm::primitives::{AccountInfo, CreateScheme};
use log::{debug, warn};
use std::collections::hash_map::RandomState as StdRandomState;

use super::{
    diff::{
        AccessType, AccountStatus, AccountValueStatus, EnhancedAccountDiff, EnhancedCodeDiff,
        EnhancedStateDiff, EnhancedStorageDiff, SlotChange, SlotStatus,
    },
    provider::WasiStateProvider,
};

pub struct StateDiffCollectorSettings {
    pub include_read_only: bool,
    
    pub include_proofs: bool,
    
    pub compression_level: CompressionLevel,
    
    pub max_diff_size: usize,
    
    pub max_entries_per_chunk: usize,
}

impl Default for StateDiffCollectorSettings {
    fn default() -> Self {
        Self {
            include_read_only: false,
            include_proofs: false,
            compression_level: CompressionLevel::Medium,
            max_diff_size: 20 * 1024 * 1024, 
            max_entries_per_chunk: 10000,    
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
struct AccountChangeTracker {
    original: Option<AccountInfo>,
    
    current: Option<AccountInfo>,
    
    created: bool,
    
    deleted: bool,
    
    accessed: bool,
}

impl AccountChangeTracker {
    pub fn new(account: Option<AccountInfo>, created: bool) -> Self {
        Self {
            original: account.clone(),
            current: account,
            created,
            deleted: false,
            accessed: true,
        }
    }
    
    pub fn record_deletion(&mut self) {
        self.deleted = true;
        self.current = None;
    }
    
    pub fn record_update(&mut self, account: AccountInfo) {
        self.current = Some(account);
        self.accessed = true;
    }
    
    pub fn status(&self) -> AccountStatus {
        if self.created {
            AccountStatus::Created
        } else if self.deleted {
            AccountStatus::Deleted
        } else if self.has_changes() {
            AccountStatus::Modified
        } else if self.accessed {
            AccountStatus::Touched
        } else {
            AccountStatus::Touched
        }
    }
    
    pub fn has_changes(&self) -> bool {
        if self.created || self.deleted {
            return true;
        }
        
        match (&self.original, &self.current) {
            (Some(orig), Some(curr)) => {
                orig.balance != curr.balance 
                || orig.nonce != curr.nonce 
                || orig.code_hash != curr.code_hash
            }
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
        }
    }
    
    pub fn to_diff(&self, address: Address) -> EnhancedAccountDiff {
        let mut diff = EnhancedAccountDiff {
            address,
            balance: None,
            nonce: None,
            code_hash: None,
            status: self.status(),
            storage_root: None,
            proof: None,
        };
        
        match (&self.original, &self.current) {
            (Some(orig), Some(curr)) => {
                if orig.balance != curr.balance {
                    let status = if self.created {
                        AccountValueStatus::Created
                    } else {
                        AccountValueStatus::Modified(orig.balance.to_string())
                    };
                    diff.balance = Some((curr.balance, status));
                }
                
                if orig.nonce != curr.nonce {
                    let status = if self.created {
                        AccountValueStatus::Created
                    } else {
                        AccountValueStatus::Modified(orig.nonce.to_string())
                    };
                    diff.nonce = Some((curr.nonce, status));
                }
                
                if orig.code_hash != curr.code_hash {
                    let status = if self.created {
                        AccountValueStatus::Created
                    } else {
                        AccountValueStatus::Modified(format!("{:?}", orig.code_hash))
                    };
                    diff.code_hash = Some((curr.code_hash, status));
                }
            }
            (None, Some(curr)) => {
                diff.balance = Some((curr.balance, AccountValueStatus::Created));
                diff.nonce = Some((curr.nonce, AccountValueStatus::Created));
                if curr.code_hash != B256::ZERO {
                    diff.code_hash = Some((curr.code_hash, AccountValueStatus::Created));
                }
            }
            (Some(orig), None) => {
                diff.balance = Some((U256::ZERO, AccountValueStatus::Deleted));
                diff.nonce = Some((0, AccountValueStatus::Deleted));
                if orig.code_hash != B256::ZERO {
                    diff.code_hash = Some((B256::ZERO, AccountValueStatus::Deleted));
                }
            }
            (None, None) => {
                warn!("Account tracker for {:?} has no original or current state", address);
            }
        }
        
        diff
    }
}

#[derive(Debug, Clone)]
struct StorageChangeTracker {
    original: B256,
    
    current: B256,
    
    created: bool,
    
    deleted: bool,
    
    accessed: bool,
}

impl StorageChangeTracker {
    pub fn new(value: B256) -> Self {
        Self {
            original: value,
            current: value,
            created: value == B256::ZERO,
            deleted: false,
            accessed: true,
        }
    }
    
    pub fn record_write(&mut self, value: B256) {
        if value == B256::ZERO {
            self.deleted = true;
        }
        self.current = value;
        self.accessed = true;
    }
    
    pub fn record_read(&mut self) {
        self.accessed = true;
    }
    
    pub fn has_changes(&self) -> bool {
        self.created || self.deleted || self.original != self.current
    }
    
    pub fn status(&self) -> SlotStatus {
        if self.created && self.current != B256::ZERO {
            SlotStatus::Created
        } else if self.deleted || self.current == B256::ZERO {
            SlotStatus::Deleted
        } else if self.original != self.current {
            SlotStatus::Modified
        } else if self.accessed {
            SlotStatus::Accessed
        } else {
            SlotStatus::Accessed
        }
    }
    
    pub fn to_slot_change(&self, slot: B256) -> SlotChange {
        SlotChange {
            slot,
            new_value: self.current,
            old_value: Some(self.original),
            status: self.status(),
        }
    }
}

#[derive(Debug, Clone)]
struct CodeChangeTracker {
    bytecode: alloy_primitives::Bytes,
    
    created: bool,
}

impl CodeChangeTracker {
    pub fn new(bytecode: alloy_primitives::Bytes, created: bool) -> Self {
        Self {
            bytecode,
            created,
        }
    }
}

pub struct StateDiffCollector {
    pub account_changes: HashMap<Address, AccountChangeTracker, StdRandomState>,
    
    pub storage_changes: HashMap<Address, HashMap<B256, StorageChangeTracker, StdRandomState>, StdRandomState>,
    
    pub code_changes: HashMap<B256, CodeChangeTracker, StdRandomState>,
    
    pub accesses: HashSet<(Address, AccessType), StdRandomState>,
    
    pub settings: StateDiffCollectorSettings,
}

impl StateDiffCollector {
    pub fn new(settings: StateDiffCollectorSettings) -> Self {
        Self {
            account_changes: HashMap::with_hasher(StdRandomState::new()),
            storage_changes: HashMap::with_hasher(StdRandomState::new()),
            code_changes: HashMap::with_hasher(StdRandomState::new()),
            accesses: HashSet::with_hasher(StdRandomState::new()),
            settings,
        }
    }
    
    pub fn record_account_access(&mut self, address: &Address, access_type: AccessType) {
        self.accesses.insert((*address, access_type));
        
        if access_type.is_write() {
            self.ensure_account_tracked(address);
        }
    }
    
    pub fn ensure_account_tracked(&mut self, address: &Address) {
        if !self.account_changes.contains_key(address) {
            self.account_changes.insert(
                *address,
                AccountChangeTracker::new(None, false)
            );
        }
    }
    
    pub fn record_account_create(
        &mut self,
        address: &Address,
        _scheme: &CreateScheme,
        account: AccountInfo,
    ) {
        debug!("Recording account creation: {:?}", address);
        self.accesses.insert((*address, AccessType::Create));
        
        self.account_changes.insert(
            *address,
            AccountChangeTracker::new(Some(account), true)
        );
    }
    
    pub fn record_account_update(&mut self, address: &Address, account: AccountInfo) {
        debug!("Recording account update: {:?}", address);
        self.accesses.insert((*address, AccessType::Write));
        
        if let Some(tracker) = self.account_changes.get_mut(address) {
            tracker.record_update(account);
        } else {
            self.account_changes.insert(
                *address,
                AccountChangeTracker::new(Some(account.clone()), false)
            );
        }
    }
    
    pub fn record_account_delete(&mut self, address: &Address) {
        debug!("Recording account deletion: {:?}", address);
        self.accesses.insert((*address, AccessType::Delete));
        
        if let Some(tracker) = self.account_changes.get_mut(address) {
            tracker.record_deletion();
        } else {
            let mut tracker = AccountChangeTracker::new(None, false);
            tracker.record_deletion();
            self.account_changes.insert(*address, tracker);
        }
    }
    
    pub fn record_storage_update(
        &mut self,
        address: &Address,
        slot: B256,
        value: B256,
        prev_value: B256,
    ) {
        debug!("Recording storage update: {:?}:{:?} = {:?}", address, slot, value);
        self.accesses.insert((*address, AccessType::Write));
        
        let account_storage = self.storage_changes
            .entry(*address)
            .or_insert_with(|| HashMap::with_hasher(StdRandomState::new()));
            
        let tracker = account_storage
            .entry(slot)
            .or_insert_with(|| StorageChangeTracker::new(prev_value));
            
        tracker.record_write(value);
    }
    
    pub fn record_storage_read(&mut self, address: &Address, slot: B256, value: B256) {
        debug!("Recording storage read: {:?}:{:?}", address, slot);
        self.accesses.insert((*address, AccessType::Read));
        
        let account_storage = self.storage_changes
            .entry(*address)
            .or_insert_with(|| HashMap::with_hasher(StdRandomState::new()));
            
        let tracker = account_storage
            .entry(slot)
            .or_insert_with(|| StorageChangeTracker::new(value));
            
        tracker.record_read();
    }
    
    pub fn record_code_change(
        &mut self,
        code_hash: B256,
        bytecode: alloy_primitives::Bytes,
        created: bool,
    ) {
        debug!("Recording code change: {:?}", code_hash);
        
        if !self.code_changes.contains_key(&code_hash) {
            self.code_changes.insert(
                code_hash,
                CodeChangeTracker::new(bytecode, created)
            );
        }
    }
    
    pub fn estimated_changes_size(&self) -> usize {
        let accounts_size = self.account_changes.len() * std::mem::size_of::<AccountChangeTracker>();
        let storage_size = self.storage_changes.values()
            .map(|map| map.len() * std::mem::size_of::<StorageChangeTracker>())
            .sum::<usize>();
        let code_size = self.code_changes.values()
            .map(|tracker| std::mem::size_of::<CodeChangeTracker>() + tracker.bytecode.len())
            .sum::<usize>();
        
        accounts_size + storage_size + code_size
    }
    
    pub fn can_include_reads(&self, current_size: usize) -> bool {
        self.settings.include_read_only && 
        current_size < self.settings.max_diff_size / 2
    }
    
    pub fn can_include_proofs(&self, current_size: usize) -> bool {
        self.settings.include_proofs && 
        current_size < self.settings.max_diff_size / 2
    }
    
    pub fn generate_minimal_diff(&self, state_provider: &WasiStateProvider) -> EnhancedStateDiff {
        let mut diff = EnhancedStateDiff::new(B256::ZERO, 0); 
        
        for (address, tracker) in &self.account_changes {
            if tracker.has_changes() {
                diff.accounts.push(tracker.to_diff(*address));
            }
        }
        
        for (address, slots) in &self.storage_changes {
            let mut storage_diff = EnhancedStorageDiff {
                address: *address,
                slot_changes: Vec::new(),
                read_only_slots: None,
                proofs: None,
            };
            
            for (slot, tracker) in slots {
                if tracker.has_changes() {
                    storage_diff.slot_changes.push(tracker.to_slot_change(*slot));
                }
            }
            
            if !storage_diff.slot_changes.is_empty() {
                diff.storage.push(storage_diff);
            }
        }
        
        for (code_hash, tracker) in &self.code_changes {
            diff.code.push(EnhancedCodeDiff {
                hash: *code_hash,
                bytecode: tracker.bytecode.clone(),
            });
        }
        
        for (address, tracker) in &self.account_changes {
            if tracker.deleted {
                diff.deleted_accounts.push(*address);
            }
        }
        
        for (address, slots) in &self.storage_changes {
            for (slot, tracker) in slots {
                if tracker.status() == SlotStatus::Deleted {
                    diff.deleted_storage.push((*address, *slot));
                }
            }
        }
        
        diff
    }
    
    pub fn enhance_with_reads(
        &self,
        mut diff: EnhancedStateDiff,
        state_provider: &WasiStateProvider,
    ) -> EnhancedStateDiff {
        for (address, access_type) in &self.accesses {
            if *access_type == AccessType::Read && !self.account_changes.contains_key(address) {
                if let Some(account_info) = state_provider.account_info(*address) {
                    let tracker = AccountChangeTracker::new(Some(account_info), false);
                    diff.accounts.push(tracker.to_diff(*address));
                }
            }
        }
        
        for (address, slots) in &self.storage_changes {
            let mut read_only_slots = Vec::new();
            for (slot, tracker) in slots {
                if !tracker.has_changes() && tracker.accessed {
                    read_only_slots.push(*slot);
                }
            }
            
            if !read_only_slots.is_empty() {
                let storage_diff = diff.storage.iter_mut()
                    .find(|s| s.address == *address);
                
                if let Some(storage_diff) = storage_diff {
                    storage_diff.read_only_slots = Some(read_only_slots);
                } else {
                    diff.storage.push(EnhancedStorageDiff {
                        address: *address,
                        slot_changes: Vec::new(),
                        read_only_slots: Some(read_only_slots),
                        proofs: None,
                    });
                }
            }
        }
        
        diff
    }
    
    pub fn enhance_with_proofs(
        &self,
        mut diff: EnhancedStateDiff,
        state_provider: &WasiStateProvider,
    ) -> EnhancedStateDiff {
        debug!("Generating Merkle proofs for state diff");
        
        for account in &mut diff.accounts {
            if let Some(proof) = state_provider.get_account_proof(&account.address) {
                account.proof = Some(proof);
                
                if let Some(storage_root) = state_provider.get_storage_root(&account.address) {
                    account.storage_root = Some(storage_root);
                }
            }
        }
        
        for storage_diff in &mut diff.storage {
            let address = storage_diff.address;
            
            let mut keys_to_prove: Vec<B256> = storage_diff.slot_changes.iter()
                .map(|change| change.slot)
                .collect();
            
            if let Some(read_only) = &storage_diff.read_only_slots {
                keys_to_prove.extend(read_only);
            }
            
            keys_to_prove.sort();
            keys_to_prove.dedup();
            
            let mut storage_proofs = HashMap::with_hasher(StdRandomState::new());
            
            for key in keys_to_prove {
                if let Some(proof) = state_provider.get_storage_proof(&address, &key) {
                    storage_proofs.insert(key, proof);
                }
            }
            
            if !storage_proofs.is_empty() {
                storage_diff.proofs = Some(storage_proofs.into_iter().collect());
            }
        }
        
        debug!("Added proofs for {} accounts and {} storage entries", 
            diff.accounts.iter().filter(|a| a.proof.is_some()).count(),
            diff.storage.iter().filter(|s| s.proofs.is_some()).count());
        
        diff
    }
    
    pub fn compress_diff(&self, diff: EnhancedStateDiff) -> EnhancedStateDiff {
        match self.settings.compression_level {
            CompressionLevel::None => diff,
            CompressionLevel::Low => {
                let mut compressed = diff;
                
                for storage_diff in &mut compressed.storage {
                    storage_diff.read_only_slots = None;
                }
                
                compressed
            }
            CompressionLevel::Medium | CompressionLevel::High => {
                let mut compressed = diff;
                
                for storage_diff in &mut compressed.storage {
                    storage_diff.read_only_slots = None;
                }
                
                compressed.accounts.retain(|a| a.status != AccountStatus::Touched);
                
                compressed
            }
        }
    }
    
    pub fn generate_diff(
        &self,
        state_provider: &WasiStateProvider,
        state_root: B256,
        block_number: u64
    ) -> EnhancedStateDiff {
        debug!("Generating state diff with {} account changes, {} storage slot changes, and {} code changes",
            self.account_changes.len(),
            self.storage_changes.values().map(|m| m.len()).sum::<usize>(),
            self.code_changes.len());
        
        let mut diff = self.generate_minimal_diff(state_provider);
        diff.state_root = state_root;
        diff.generated_at_block = block_number;
        
        if self.can_include_reads(diff.estimated_size()) {
            diff = self.enhance_with_reads(diff, state_provider);
        }
        
        if self.can_include_proofs(diff.estimated_size()) {
            diff = self.enhance_with_proofs(diff, state_provider);
        }
        
        if diff.estimated_size() > self.settings.max_diff_size {
            diff = self.compress_diff(diff);
        }
        
        if diff.estimated_size() > self.settings.max_diff_size {
            let diff_clone = diff.clone();
            let chunks = diff_clone.split_into_chunks(self.settings.max_diff_size);
            debug!("Split state diff into {} chunks", chunks.len());
            
            if !chunks.is_empty() {
                return chunks[0].clone();
            }
        }
        
        diff
    }
}