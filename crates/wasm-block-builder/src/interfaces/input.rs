use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxType {
    Legacy = 0x0,
    AccessList = 0x1,
    EIP1559 = 0x2,
    Blob = 0x3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderInput {
    pub block_params: BlockParams,
    
    pub accounts: Vec<SerializedAccount>,
    
    pub storage: Vec<SerializedStorage>,
    
    pub code: Vec<SerializedCode>,
    
    pub transactions: Vec<SerializedTransaction>,
    
    pub bundles: Vec<SerializedBundle>,
    
    pub config: BlockBuilderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockParams {
    pub number: u64,
    
    pub timestamp: u64,
    
    pub gas_limit: u64,
    
    pub base_fee_per_gas: U256,
    
    pub coinbase: Address,
    
    pub parent_hash: B256,
    
    pub parent_state_root: B256,
    
    pub withdrawals_root: Option<B256>,
    
    pub blob_gas_used: Option<u64>,
    
    pub excess_blob_gas: Option<u64>,
    
    pub parent_beacon_block_root: Option<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedAccount {
    pub address: Address,
    
    pub balance: U256,
    
    pub nonce: u64,
    
    pub code_hash: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedStorage {
    pub address: Address,
    
    pub slot: B256,
    
    pub value: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedCode {
    pub hash: B256,
    
    pub bytecode: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedTransaction {
    pub hash: B256,
    
    pub from: Address,
    
    pub to: Option<Address>,
    
    pub value: U256,
    
    pub gas_limit: u64,
    
    pub gas_price: U256,
    
    pub max_priority_fee_per_gas: Option<U256>,
    
    pub nonce: u64,
    
    pub input: Bytes,
    
    pub tx_type: TxType,
    
    pub access_list: Vec<SerializedAccessListEntry>,
    
    pub blob_hashes: Vec<B256>,
    
    pub max_fee_per_blob_gas: Option<U256>,
    
    pub versioned_hashes: Vec<B256>,
    
    pub encoded_signed_tx: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedAccessListEntry {
    pub address: Address,
    
    pub slots: Vec<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedBundle {
    pub hash: B256,
    
    pub transactions: Vec<SerializedTransaction>,
    
    pub revertible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderConfig {
    pub discard_txs: bool,
    
    pub sorting: SortingAlgorithm,
    
    pub failed_tx_retries: usize,
    
    pub drop_failed_txs: bool,
    
    pub coinbase_payment: bool,
    
    pub build_timeout_ms: Option<u64>,
    
    #[serde(default)]
    pub complete_state_diff: bool,
    
    #[serde(default)]
    pub include_merkle_proofs: bool,
    
    #[serde(default = "default_compression_level")]
    pub compression_level: String,
}

fn default_compression_level() -> String {
    "medium".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateProviderInput {
    pub accounts: Vec<SerializedAccount>,
    
    pub storage: Vec<SerializedStorage>,
    
    pub code: Vec<SerializedCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortingAlgorithm {
    GasPrice,
    
    Profit,
    
    MevGasPrice,
}