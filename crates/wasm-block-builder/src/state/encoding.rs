use alloy_primitives::{Address, B256, U256};
use hashbrown::HashMap;
use std::collections::hash_map::RandomState as StdRandomState;
use std::io::Write;
use log::debug;



use super::diff::{
    EnhancedAccountDiff, EnhancedCodeDiff, EnhancedStateDiff, EnhancedStorageDiff,
    SlotChange, SlotStatus,
};
use super::collector::CompressionLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingMethod {
    AccountFocused,
    
    StorageFocused,
    
    Balanced,
}

#[derive(Debug, Clone)]
pub struct EncodedStorageDiff {
    pub address: Address,
    
    pub encoded_slots: Vec<EncodedSlotChange>,
}

#[derive(Debug, Clone)]
pub struct EncodedSlotChange {
    pub slot_delta: B256,
    
    pub new_value: B256,
    
    pub old_value: Option<B256>,
    
    pub status: SlotStatus,
}

pub struct DiffEncoder {
    pub compression_level: CompressionLevel,
    
    pub use_dictionary_encoding: bool,
}

impl DiffEncoder {
    pub fn new(compression_level: CompressionLevel) -> Self {
        Self {
            compression_level,
            use_dictionary_encoding: compression_level != CompressionLevel::None,
        }
    }
    
    pub fn select_encoding_method(&self, diff: &EnhancedStateDiff) -> EncodingMethod {
        if diff.is_mostly_accounts() {
            EncodingMethod::AccountFocused
        } else if diff.is_mostly_storage() {
            EncodingMethod::StorageFocused
        } else {
            EncodingMethod::Balanced
        }
    }
    
    pub fn encode_diff(&self, diff: EnhancedStateDiff) -> Vec<u8> {
        let method = self.select_encoding_method(&diff);
        debug!("Selected encoding method: {:?}", method);
        
        let optimized_diff = self.preprocess_diff(diff, method);
        
        match method {
            EncodingMethod::AccountFocused => self.encode_account_focused(&optimized_diff),
            EncodingMethod::StorageFocused => self.encode_storage_focused(&optimized_diff),
            EncodingMethod::Balanced => self.encode_balanced(&optimized_diff),
        }
    }
    
    fn preprocess_diff(&self, diff: EnhancedStateDiff, method: EncodingMethod) -> EnhancedStateDiff {
        match method {
            EncodingMethod::AccountFocused => {
                let mut optimized = diff.clone();
                
                optimized.accounts.sort_by(|a, b| a.address.cmp(&b.address));
                
                optimized
            },
            EncodingMethod::StorageFocused => {
                let mut optimized = diff.clone();
                
                optimized.storage.sort_by(|a, b| a.address.cmp(&b.address));
                
                for storage_diff in &mut optimized.storage {
                    storage_diff.slot_changes.sort_by(|a, b| a.slot.cmp(&b.slot));
                }
                
                optimized
            },
            EncodingMethod::Balanced => {
                let mut optimized = diff.clone();
                
                optimized.accounts.sort_by(|a, b| a.address.cmp(&b.address));
                optimized.storage.sort_by(|a, b| a.address.cmp(&b.address));
                
                for storage_diff in &mut optimized.storage {
                    storage_diff.slot_changes.sort_by(|a, b| a.slot.cmp(&b.slot));
                }
                
                optimized
            },
        }
    }
    
    fn encode_account_focused(&self, diff: &EnhancedStateDiff) -> Vec<u8> {
        let mut result = Vec::new();
        
        result.extend_from_slice(&[1u8, 0u8, 0u8, 0u8]); 
        result.extend_from_slice(&[1u8, 0u8, 0u8, 0u8]); 
        
        result.extend_from_slice(diff.state_root.as_slice());
        
        result.extend_from_slice(&diff.generated_at_block.to_be_bytes());
        
        result.extend_from_slice(&(diff.accounts.len() as u32).to_be_bytes());
        
        result.extend_from_slice(&(diff.storage.len() as u32).to_be_bytes());
        
        result.extend_from_slice(&(diff.code.len() as u32).to_be_bytes());
        
        for account in &diff.accounts {
            result.extend_from_slice(account.address.as_slice());
            
            result.push(account.status as u8);
            
            if let Some((balance, status)) = &account.balance {
                result.push(1u8); 
                result.extend_from_slice(&balance.to_be_bytes());
                result.push(status_to_u8(status));
            } else {
                result.push(0u8); 
            }
            
            if let Some((nonce, status)) = &account.nonce {
                result.push(1u8); 
                result.extend_from_slice(&nonce.to_be_bytes());
                result.push(status_to_u8(status));
            } else {
                result.push(0u8); 
            }
            
            if let Some((code_hash, status)) = &account.code_hash {
                result.push(1u8); 
                result.extend_from_slice(code_hash.as_slice());
                result.push(status_to_u8(status));
            } else {
                result.push(0u8); 
            }
        }
        
        for storage in &diff.storage {
            result.extend_from_slice(storage.address.as_slice());
            
            result.extend_from_slice(&(storage.slot_changes.len() as u32).to_be_bytes());
            
            for slot in &storage.slot_changes {
                result.extend_from_slice(slot.slot.as_slice());
                result.extend_from_slice(slot.new_value.as_slice());
                result.push(slot.status as u8);
            }
        }
        
        for code in &diff.code {
            result.extend_from_slice(code.hash.as_slice());
            
            result.extend_from_slice(&(code.bytecode.len() as u32).to_be_bytes());
            result.extend_from_slice(&code.bytecode);
        }
        
        debug!("Encoded state diff using account-focused encoding: {} bytes", result.len());
        
        result
    }
    
    fn status_to_u8(status: &AccountValueStatus) -> u8 {
        match status {
            AccountValueStatus::Unchanged => 0,
            AccountValueStatus::Modified(_) => 1,
            AccountValueStatus::Deleted => 2,
            AccountValueStatus::Created => 3,
        }
    }
    
    fn encode_storage_focused(&self, diff: &EnhancedStateDiff) -> Vec<u8> {
        let delta_encoded_storage = self.delta_encode_storage(&diff.storage);
        
        let mut result = Vec::new();
        
        result.extend_from_slice(&[1u8, 0u8, 0u8, 0u8]); 
        result.extend_from_slice(&[2u8, 0u8, 0u8, 0u8]); 
        
        result.extend_from_slice(diff.state_root.as_slice());
        
        result.extend_from_slice(&diff.generated_at_block.to_be_bytes());
        
        result.extend_from_slice(&(diff.accounts.len() as u32).to_be_bytes());
        
        result.extend_from_slice(&(delta_encoded_storage.len() as u32).to_be_bytes());
        
        result.extend_from_slice(&(diff.code.len() as u32).to_be_bytes());
        
        for account in &diff.accounts {
            result.extend_from_slice(account.address.as_slice());
            
            result.push(account.status as u8);
        }
        
        for storage in &delta_encoded_storage {
            result.extend_from_slice(storage.address.as_slice());
            
            result.extend_from_slice(&(storage.encoded_slots.len() as u32).to_be_bytes());
            
            for slot in &storage.encoded_slots {
                result.extend_from_slice(slot.slot_delta.as_slice());
                
                result.extend_from_slice(slot.new_value.as_slice());
                
                if let Some(old_value) = slot.old_value {
                    result.push(1u8); 
                    result.extend_from_slice(old_value.as_slice());
                } else {
                    result.push(0u8); 
                }
                
                result.push(slot.status as u8);
            }
        }
        
        for code in &diff.code {
            result.extend_from_slice(code.hash.as_slice());
            
            let prefix_len = std::cmp::min(64, code.bytecode.len());
            result.extend_from_slice(&(prefix_len as u32).to_be_bytes());
            result.extend_from_slice(&code.bytecode[..prefix_len]);
        }
        
        debug!("Encoded state diff using storage-focused encoding: {} bytes", result.len());
        
        result
    }
    
    fn encode_balanced(&self, diff: &EnhancedStateDiff) -> Vec<u8> {
        let delta_encoded_storage = self.delta_encode_storage(&diff.storage);
        
        let mut result = Vec::new();
        
        result.extend_from_slice(&[1u8, 0u8, 0u8, 0u8]); 
        result.extend_from_slice(&[3u8, 0u8, 0u8, 0u8]); 
        
        result.extend_from_slice(diff.state_root.as_slice());
        
        result.extend_from_slice(&diff.generated_at_block.to_be_bytes());
        
        let frequent_addresses = self.find_frequent_addresses(diff);
        let use_dict = !frequent_addresses.is_empty();
        
        result.push(if use_dict { 1u8 } else { 0u8 });
        
        if use_dict {
            result.extend_from_slice(&(frequent_addresses.len() as u32).to_be_bytes());
            
            for (i, addr) in frequent_addresses.iter().enumerate() {
                result.extend_from_slice(&(i as u16).to_be_bytes()); 
                result.extend_from_slice(addr.as_slice()); 
            }
        }
        
        result.extend_from_slice(&(diff.accounts.len() as u32).to_be_bytes());
        result.extend_from_slice(&(delta_encoded_storage.len() as u32).to_be_bytes());
        result.extend_from_slice(&(diff.code.len() as u32).to_be_bytes());
        
        for account in &diff.accounts {
            if use_dict {
                let position = frequent_addresses.iter().position(|a| a == &account.address);
                if let Some(idx) = position {
                    result.push(1u8); 
                    result.extend_from_slice(&(idx as u16).to_be_bytes());
                } else {
                    result.push(0u8); 
                    result.extend_from_slice(account.address.as_slice());
                }
            } else {
                result.extend_from_slice(account.address.as_slice());
            }
            
            result.push(account.status as u8);
            
            match account.status {
                AccountStatus::Created | AccountStatus::Modified => {
                    if let Some((balance, status)) = &account.balance {
                        result.push(1u8); 
                        result.extend_from_slice(&balance.to_be_bytes());
                        result.push(self.status_to_u8(status));
                    } else {
                        result.push(0u8); 
                    }
                    
                    if let Some((nonce, status)) = &account.nonce {
                        result.push(1u8); 
                        result.extend_from_slice(&nonce.to_be_bytes());
                        result.push(self.status_to_u8(status));
                    } else {
                        result.push(0u8); 
                    }
                    
                    if let Some((code_hash, status)) = &account.code_hash {
                        result.push(1u8); 
                        result.extend_from_slice(code_hash.as_slice());
                        result.push(self.status_to_u8(status));
                    } else {
                        result.push(0u8); 
                    }
                },
                _ => {
                    result.push(0u8); 
                }
            }
        }
        
        for storage in &delta_encoded_storage {
            if use_dict {
                let position = frequent_addresses.iter().position(|a| a == &storage.address);
                if let Some(idx) = position {
                    result.push(1u8); 
                    result.extend_from_slice(&(idx as u16).to_be_bytes());
                } else {
                    result.push(0u8); 
                    result.extend_from_slice(storage.address.as_slice());
                }
            } else {
                result.extend_from_slice(storage.address.as_slice());
            }
            
            result.extend_from_slice(&(storage.encoded_slots.len() as u32).to_be_bytes());
            
            for slot in &storage.encoded_slots {
                result.extend_from_slice(slot.slot_delta.as_slice());
                
                result.extend_from_slice(slot.new_value.as_slice());
                
                if slot.status == SlotStatus::Modified || slot.status == SlotStatus::Created {
                    if let Some(old_value) = slot.old_value {
                        result.push(1u8); 
                        result.extend_from_slice(old_value.as_slice());
                    } else {
                        result.push(0u8); 
                    }
                }
                
                result.push(slot.status as u8);
            }
        }
        
        for code in &diff.code {
            result.extend_from_slice(code.hash.as_slice());
            
            result.extend_from_slice(&(code.bytecode.len() as u32).to_be_bytes());
            
            let code_len = if code.bytecode.len() <= 1024 {
                code.bytecode.len() 
            } else {
                std::cmp::min(256, code.bytecode.len()) 
            };
            
            result.extend_from_slice(&code.bytecode[..code_len]);
        }
        
        if self.compression_level != CompressionLevel::None && result.len() > 1024 {
            let mut compressed = Vec::with_capacity(result.len());
            
            compressed.extend_from_slice(&[0xC0, 0xDE, 0xC0, 0xMP]);
            
            compressed.extend_from_slice(&(result.len() as u32).to_be_bytes());
            
            let mut i = 0;
            while i < result.len() {
                let byte = result[i];
                let mut count = 1;
                
                while i + count < result.len() && result[i + count] == byte && count < 255 {
                    count += 1;
                }
                
                if count >= 4 {
                    compressed.push(0xFF); 
                    compressed.push(byte); 
                    compressed.push(count as u8); 
                    i += count;
                } else {
                    compressed.push(byte);
                    i += 1;
                }
            }
            
            debug!("Encoded state diff using balanced encoding: original={} bytes, compressed={} bytes", 
                  result.len(), compressed.len());
            
            if compressed.len() < result.len() {
                return compressed;
            }
        }
        
        debug!("Encoded state diff using balanced encoding: {} bytes (uncompressed)", result.len());
        result
    }
    
    fn find_frequent_addresses(&self, diff: &EnhancedStateDiff) -> Vec<Address> {
        let mut address_counts = HashMap::with_hasher(StdRandomState::new());
        
        for account in &diff.accounts {
            *address_counts.entry(account.address).or_insert(0) += 1;
        }
        
        for storage in &diff.storage {
            *address_counts.entry(storage.address).or_insert(0) += 1;
        }
        
        let mut frequent: Vec<(Address, usize)> = address_counts.into_iter()
            .filter(|(_, count)| *count > 1) 
            .collect();
        
        frequent.sort_by(|a, b| b.1.cmp(&a.1));
        
        let top_addresses: Vec<Address> = frequent.into_iter()
            .take(std::cmp::min(frequent.len(), u16::MAX as usize))
            .map(|(addr, _)| addr)
            .collect();
        
        top_addresses
    }
    
    pub fn delta_encode_storage(&self, storage_diff: &[EnhancedStorageDiff]) -> Vec<EncodedStorageDiff> {
        let mut by_address: HashMap<Address, Vec<SlotChange>, StdRandomState> = 
            HashMap::with_hasher(StdRandomState::new());
        
        for diff in storage_diff {
            let slots = by_address.entry(diff.address).or_default();
            slots.extend(diff.slot_changes.clone());
        }
        
        for slots in by_address.values_mut() {
            slots.sort_by(|a, b| a.slot.cmp(&b.slot));
        }
        
        by_address
            .into_iter()
            .map(|(address, slots)| {
                let encoded_slots = self.delta_encode_slots(&slots);
                EncodedStorageDiff { address, encoded_slots }
            })
            .collect()
    }
    
    fn delta_encode_slots(&self, slots: &[SlotChange]) -> Vec<EncodedSlotChange> {
        if slots.is_empty() {
            return Vec::new();
        }
        
        let mut result = Vec::with_capacity(slots.len());
        let mut last_slot = B256::ZERO;
        
        for (i, slot) in slots.iter().enumerate() {
            let slot_delta = if i == 0 {
                slot.slot
            } else {
                let slot_u256 = U256::from_be_bytes(slot.slot.into());
                let last_u256 = U256::from_be_bytes(last_slot.into());
                let delta_u256 = slot_u256 - last_u256;
                B256::from(delta_u256)
            };
            
            last_slot = slot.slot;
            
            result.push(EncodedSlotChange {
                slot_delta,
                new_value: slot.new_value,
                old_value: slot.old_value,
                status: slot.status,
            });
        }
        
        result
    }
    
    fn dictionary_encode_addresses(&self, diff: &EnhancedStateDiff) 
        -> (Vec<u8>, HashMap<Address, u16, StdRandomState>) 
    {
        let mut address_counts: HashMap<Address, usize, StdRandomState> = 
            HashMap::with_hasher(StdRandomState::new());
        
        for account in &diff.accounts {
            *address_counts.entry(account.address).or_default() += 1;
        }
        
        for storage in &diff.storage {
            *address_counts.entry(storage.address).or_default() += 1;
        }
        
        let mut addresses: Vec<(Address, usize)> = address_counts.into_iter().collect();
        addresses.sort_by(|a, b| b.1.cmp(&a.1)); 
        
        let mut addre1ss_dict: HashMap<Address, u16, StdRandomState> =
            HashMap::with_hasher(StdRandomState::new());
        
        for (i, (address, _)) in addresses.iter().enumerate().take(u16::MAX as usize) {
            address_dict.insert(*address, i as u16);
        }
        
        let encoded = serde_json::to_vec(&address_dict).unwrap_or_default();
        
        (encoded, address_dict)
    }
}