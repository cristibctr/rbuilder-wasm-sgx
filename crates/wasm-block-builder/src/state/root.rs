use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_rlp::{RlpEncodable, RlpDecodable, Encodable, Header};
use crate::interfaces::output::{SerializedAccountDiff, SerializedStateDiff, SerializedStorageDiff};
use crate::state::WasiStateProvider;
use hashbrown::{HashMap, HashSet};
use std::collections::hash_map::RandomState as StdRandomState;
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

type Nibbles = Vec<u8>;

fn bytes_to_nibbles(bytes: &[u8]) -> Nibbles {
    let mut nibbles = Vec::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        nibbles.push(byte >> 4);
        nibbles.push(byte & 0x0F);
    }
    nibbles
}

fn nibbles_to_compact(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
    let mut compact = Vec::new();
    
    let odd_length = nibbles.len() % 2 != 0;
    
    let prefix_nibble = if is_leaf { 0x20 } else { 0x00 } + if odd_length { 0x10 } else { 0x00 };
    
    if odd_length {
        compact.push(prefix_nibble | nibbles[0]);
        for i in (1..nibbles.len()).step_by(2) {
            compact.push((nibbles[i] << 4) | nibbles[i + 1]);
        }
    } else {
        compact.push(prefix_nibble);
        for i in (0..nibbles.len()).step_by(2) {
            compact.push((nibbles[i] << 4) | nibbles[i + 1]);
        }
    }
    
    compact
}

#[derive(Debug, Clone)]
enum TrieNode {
    Empty,
    
    Leaf {
        key: Nibbles,
        value: Bytes,
    },
    
    Branch {
        children: [Option<Box<TrieNode>>; 16],
        value: Option<Bytes>,
    },
    
    Extension {
        key: Nibbles,
        next: Box<TrieNode>,
    },
}

impl AsRef<TrieNode> for TrieNode {
    fn as_ref(&self) -> &TrieNode {
        self
    }
}


fn hash_node(node: &TrieNode) -> B256 {
    if let Some(encoded) = rlp_encode_node(node) {
        if encoded.len() < 32 {
            return B256::from_slice(keccak256(&encoded).as_ref());
        }
        B256::from_slice(keccak256(&encoded).as_ref())
    } else {
        B256::ZERO
    }
}

fn rlp_encode_node(node: &TrieNode) -> Option<Vec<u8>> {
    match node {
        TrieNode::Empty => None,
        
        TrieNode::Leaf { key, value } => {
            let compact_key = nibbles_to_compact(key, true);
            
            let mut buffer = Vec::new();
            
            let mut data = Vec::new();
            data.extend_from_slice(&alloy_rlp::encode(&compact_key));
            data.extend_from_slice(&alloy_rlp::encode(value.as_ref()));
            
            Header { list: true, payload_length: data.len() }.encode(&mut buffer);
            buffer.extend_from_slice(&data);
            
            Some(buffer)
        },
        
        TrieNode::Branch { children, value } => {
            let mut data = Vec::new();
            
            for child in children.iter() {
                match child {
                    Some(node) => {
                        let encoded = rlp_encode_node(node);
                        if let Some(data_slice) = encoded {
                            if data_slice.len() >= 32 {
                                let hash = keccak256(&data_slice);
                                data.extend_from_slice(&alloy_rlp::encode(&hash));
                            } else {
                                data.extend_from_slice(&alloy_rlp::encode(&data_slice));
                            }
                        } else {
                            data.extend_from_slice(&alloy_rlp::encode(&Bytes::new()));
                        }
                    },
                    None => {
                        data.extend_from_slice(&alloy_rlp::encode(&Bytes::new()));
                    }
                }
            }
            
            match value {
                Some(v) => data.extend_from_slice(&alloy_rlp::encode(v.as_ref())),
                None => data.extend_from_slice(&alloy_rlp::encode(&Bytes::new())),
            }
            
            let mut buffer = Vec::new();
            Header { list: true, payload_length: data.len() }.encode(&mut buffer);
            buffer.extend_from_slice(&data);
            
            Some(buffer)
        },
        
        TrieNode::Extension { key, next } => {
            let compact_key = nibbles_to_compact(key, false);
            
            let mut data = Vec::new();
            
            data.extend_from_slice(&alloy_rlp::encode(&compact_key));
            
            let encoded = rlp_encode_node(next);
            if let Some(data_slice) = encoded {
                if data_slice.len() >= 32 {
                    let hash = keccak256(&data_slice);
                    data.extend_from_slice(&alloy_rlp::encode(&hash));
                } else {
                    data.extend_from_slice(&alloy_rlp::encode(&data_slice));
                }
            } else {
                data.extend_from_slice(&alloy_rlp::encode(&Bytes::new()));
            }
            
            let mut buffer = Vec::new();
            Header { list: true, payload_length: data.len() }.encode(&mut buffer);
            buffer.extend_from_slice(&data);
            
            Some(buffer)
        },
    }
}

struct PatriciaTrie {
    root: TrieNode,
    
    account_cache: HashMap<Address, Bytes, StdRandomState>,
    
    storage_cache: HashMap<(Address, B256), Bytes, StdRandomState>,
}

impl PatriciaTrie {
    fn new() -> Self {
        Self {
            root: TrieNode::Empty,
            account_cache: HashMap::with_hasher(StdRandomState::new()),
            storage_cache: HashMap::with_hasher(StdRandomState::new()),
        }
    }
    
    fn root_hash(&self) -> B256 {
        hash_node(&self.root)
    }
    
    fn insert(&mut self, key: &[u8], value: &[u8]) {
        let nibbles = bytes_to_nibbles(key);
        let root_clone = self.root.clone();
        self.root = self.insert_at(&root_clone, &nibbles, Bytes::copy_from_slice(value));
    }
    
    fn insert_at(&mut self, node: &TrieNode, nibbles: &[u8], value: Bytes) -> TrieNode {
        match node {
            TrieNode::Empty => {
                TrieNode::Leaf {
                    key: nibbles.to_vec(),
                    value,
                }
            },
            
            TrieNode::Leaf { key, value: old_value } => {
                let common_prefix = self.get_common_prefix_length(key, nibbles);
                
                if common_prefix == key.len() && common_prefix == nibbles.len() {
                    TrieNode::Leaf {
                        key: key.clone(),
                        value,
                    }
                } else if common_prefix == key.len() {
                    let mut branch = self.create_branch_with_leaf(
                        &nibbles[common_prefix..],
                        value,
                        old_value.clone(),
                    );
                    
                    if common_prefix > 0 {
                        TrieNode::Extension {
                            key: nibbles[..common_prefix].to_vec(),
                            next: Box::new(branch),
                        }
                    } else {
                        branch
                    }
                } else if common_prefix == nibbles.len() {
                    let mut branch = self.create_branch_with_leaf(
                        &key[common_prefix..],
                        old_value.clone(),
                        value,
                    );
                    
                    if common_prefix > 0 {
                        TrieNode::Extension {
                            key: nibbles[..common_prefix].to_vec(),
                            next: Box::new(branch),
                        }
                    } else {
                        branch
                    }
                } else {
                    let branch = self.create_branch_node(
                        &key[common_prefix..],
                        old_value.clone(),
                        &nibbles[common_prefix..],
                        value,
                    );
                    
                    if common_prefix > 0 {
                        TrieNode::Extension {
                            key: nibbles[..common_prefix].to_vec(),
                            next: Box::new(branch),
                        }
                    } else {
                        branch
                    }
                }
            },
            
            TrieNode::Branch { children, value: existing_value } => {
                if nibbles.is_empty() {
                    let mut new_children = children.clone();
                    TrieNode::Branch {
                        children: new_children,
                        value: Some(value),
                    }
                } else {
                    let index = nibbles[0] as usize;
                    let mut new_children = children.clone();
                    
                    match &children[index] {
                        Some(child) => {
                            new_children[index] = Some(Box::new(
                                self.insert_at(child, &nibbles[1..], value)
                            ));
                        },
                        None => {
                            new_children[index] = Some(Box::new(
                                TrieNode::Leaf {
                                    key: nibbles[1..].to_vec(),
                                    value,
                                }
                            ));
                        }
                    }
                    
                    TrieNode::Branch {
                        children: new_children,
                        value: existing_value.clone(),
                    }
                }
            },
            
            TrieNode::Extension { key, next } => {
                let common_prefix = self.get_common_prefix_length(key, nibbles);
                
                if common_prefix == key.len() {
                    TrieNode::Extension {
                        key: key.clone(),
                        next: Box::new(self.insert_at(next, &nibbles[common_prefix..], value)),
                    }
                } else if common_prefix > 0 {
                    let branch = self.handle_extension_split(
                        key,
                        next,
                        nibbles,
                        common_prefix,
                        value,
                    );
                    
                    branch
                } else {
                    let mut children = [None, None, None, None, None, None, None, None,
                                       None, None, None, None, None, None, None, None];
                    
                    let existing_index = key[0] as usize;
                    if key.len() > 1 {
                        children[existing_index] = Some(Box::new(
                            TrieNode::Extension {
                                key: key[1..].to_vec(),
                                next: Box::new(next.as_ref().clone()),
                            }
                        ));
                    } else {
                        children[existing_index] = Some(next.clone());
                    }
                    
                    if !nibbles.is_empty() {
                        let new_index = nibbles[0] as usize;
                        if nibbles.len() > 1 {
                            children[new_index] = Some(Box::new(
                                TrieNode::Leaf {
                                    key: nibbles[1..].to_vec(),
                                    value,
                                }
                            ));
                        } else {
                            return TrieNode::Branch {
                                children,
                                value: Some(value),
                            };
                        }
                    }
                    
                    TrieNode::Branch {
                        children,
                        value: None,
                    }
                }
            }
        }
    }
    
    fn create_branch_with_leaf(&self, leaf_key: &[u8], leaf_value: Bytes, branch_value: Bytes) -> TrieNode {
        let mut children = [None, None, None, None, None, None, None, None,
                           None, None, None, None, None, None, None, None];
        
        if !leaf_key.is_empty() {
            let index = leaf_key[0] as usize;
            if leaf_key.len() > 1 {
                children[index] = Some(Box::new(
                    TrieNode::Leaf {
                        key: leaf_key[1..].to_vec(),
                        value: leaf_value,
                    }
                ));
            } else {
                return TrieNode::Branch {
                    children,
                    value: Some(leaf_value),
                };
            }
        }
        
        TrieNode::Branch {
            children,
            value: Some(branch_value),
        }
    }
    
    fn create_branch_node(&self, key1: &[u8], value1: Bytes, key2: &[u8], value2: Bytes) -> TrieNode {
        let mut children = [None, None, None, None, None, None, None, None,
                           None, None, None, None, None, None, None, None];
        
        let mut branch_value = None;
        
        if !key1.is_empty() {
            let index = key1[0] as usize;
            if key1.len() > 1 {
                children[index] = Some(Box::new(
                    TrieNode::Leaf {
                        key: key1[1..].to_vec(),
                        value: value1,
                    }
                ));
            } else {
                branch_value = Some(value1);
            }
        } else {
            branch_value = Some(value1);
        }
        
        if !key2.is_empty() {
            let index = key2[0] as usize;
            if key2.len() > 1 {
                children[index] = Some(Box::new(
                    TrieNode::Leaf {
                        key: key2[1..].to_vec(),
                        value: value2,
                    }
                ));
            } else {
                branch_value = Some(value2);
            }
        } else {
            branch_value = Some(value2);
        }
        
        TrieNode::Branch {
            children,
            value: branch_value,
        }
    }
    
    fn handle_extension_split(&self, ext_key: &[u8], next: &TrieNode, nibbles: &[u8], common_prefix: usize, value: Bytes) -> TrieNode {
        let mut children = [None, None, None, None, None, None, None, None,
                           None, None, None, None, None, None, None, None];
        
        if ext_key.len() > common_prefix + 1 {
            let idx = ext_key[common_prefix] as usize;
            children[idx] = Some(Box::new(
                TrieNode::Extension {
                    key: ext_key[common_prefix+1..].to_vec(),
                    next: Box::new(next.as_ref().clone()),
                }
            ));
        } else {
            let idx = ext_key[common_prefix] as usize;
            children[idx] = Some(Box::new(next.clone()));
        }
        
        if nibbles.len() > common_prefix {
            let idx = nibbles[common_prefix] as usize;
            if nibbles.len() > common_prefix + 1 {
                children[idx] = Some(Box::new(
                    TrieNode::Leaf {
                        key: nibbles[common_prefix+1..].to_vec(),
                        value,
                    }
                ));
            } else {
                return TrieNode::Extension {
                    key: nibbles[..common_prefix].to_vec(),
                    next: Box::new(TrieNode::Branch {
                        children,
                        value: Some(value),
                    }),
                };
            }
        }
        
        TrieNode::Extension {
            key: nibbles[..common_prefix].to_vec(),
            next: Box::new(TrieNode::Branch {
                children,
                value: None,
            }),
        }
    }
    
    fn get_common_prefix_length(&self, a: &[u8], b: &[u8]) -> usize {
        let mut common_len = 0;
        let min_len = std::cmp::min(a.len(), b.len());
        
        for i in 0..min_len {
            if a[i] != b[i] {
                break;
            }
            common_len += 1;
        }
        
        common_len
    }
    
    fn build_account_trie(&mut self, state: &WasiStateProvider, changes: &SerializedStateDiff) -> Result<(), StateRootError> {
        for account_diff in &changes.accounts {
            let address = account_diff.address;
            
            let account_balance = account_diff.new_balance.unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.balance)
                    .unwrap_or(U256::ZERO)
            });
            
            let account_nonce = account_diff.new_nonce.unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.nonce)
                    .unwrap_or(0)
            });
            
            let account_code_hash = account_diff.new_code_hash.unwrap_or_else(|| {
                state.account_info(address)
                    .map(|info| info.code_hash)
                    .unwrap_or(B256::ZERO)
            });
            
            let storage_root = self.build_storage_trie(state, changes, address)?;
            
            let mut data = Vec::new();
            data.extend_from_slice(&alloy_rlp::encode(&account_nonce));
            data.extend_from_slice(&alloy_rlp::encode(&account_balance));
            data.extend_from_slice(&alloy_rlp::encode(&storage_root));
            data.extend_from_slice(&alloy_rlp::encode(&account_code_hash));
            
            let mut buffer = Vec::new();
            Header { list: true, payload_length: data.len() }.encode(&mut buffer);
            buffer.extend_from_slice(&data);
            
            let account_rlp = Bytes::from(buffer);
            
            let address_hash = keccak256(address.to_vec().as_slice());
            
            self.account_cache.insert(address, account_rlp.clone());
            self.insert(address_hash.as_ref(), &account_rlp);
        }
        
        
        Ok(())
    }
    
    fn build_storage_trie(&mut self, state: &WasiStateProvider, changes: &SerializedStateDiff, address: Address) -> Result<B256, StateRootError> {
        let mut storage_trie = PatriciaTrie::new();
        
        for storage_diff in &changes.storage {
            if storage_diff.address == address {
                let slot = storage_diff.slot;
                let value = storage_diff.new_value;
                
                if value == B256::ZERO {
                    continue;
                }
                
                let slot_hash = keccak256(slot.to_vec().as_slice());
                
                let mut buffer = Vec::new();
                value.encode(&mut buffer);
                let value_rlp = buffer;
                
                storage_trie.insert(slot_hash.as_ref(), &value_rlp);
                
                self.storage_cache.insert((address, slot), Bytes::from(value_rlp));
            }
        }
        
        
        Ok(storage_trie.root_hash())
    }
}

pub fn calculate_state_root(
    changes: &SerializedStateDiff, 
    state: &WasiStateProvider
) -> Result<B256, StateRootError> {
    let error_msg = format!("Calculating state root with Merkle Patricia Trie");
    log::info!("{}", error_msg);
    sgx_log(&error_msg);

    
    let mut trie = PatriciaTrie::new();
    
    trie.build_account_trie(state, changes)?;
    
    Ok(trie.root_hash())
}