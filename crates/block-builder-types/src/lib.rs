use alloy_primitives::{Address, Bytes, B256, U256, Log, Bloom};
use alloy_consensus::{Header, TxType, Eip658Value};
use alloy_eips::eip2930::AccessListItem;
use alloy_eips::eip4895::Withdrawal;
use serde::{Deserialize, Serialize};

#[cfg(feature = "encoding")]
use alloy_eips::eip2718::{Encodable2718, Typed2718};
#[cfg(feature = "encoding")]
use alloy_rlp::{BufMut, Encodable};

pub mod serde_utils;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderOutput {
    pub header: Header,
    pub transactions: Vec<Bytes>,
    pub receipts: Vec<SerializedReceipt>,
    pub state_diff: SerializedStateDiff,
    pub state_root: Option<B256>,
    pub metrics: BlockMetrics,
    pub signature: Option<Bytes>,
    pub chunk_info: Option<ChunkInfo>,
    pub build_id: Option<String>,
    pub execution_requests: Option<Vec<Bytes>>,
}

pub type SerializedReceipt = reth_primitives::Receipt;

pub type SerializedLog = Log;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedStateDiff {
    pub accounts: Vec<SerializedAccountDiff>,
    pub storage: Vec<SerializedStorageDiff>,
    pub code: Vec<SerializedCodeDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedAccountDiff {
    pub address: Address,
    pub old_balance: Option<U256>,
    pub new_balance: Option<U256>,
    pub old_nonce: Option<u64>,
    pub new_nonce: Option<u64>,
    pub old_code_hash: Option<B256>,
    pub new_code_hash: Option<B256>,
}

pub type AccountDiff = SerializedAccountDiff;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedStorageDiff {
    pub address: Address,
    pub slot: B256,
    pub old_value: B256,
    pub new_value: B256,
}

pub type StorageDiff = SerializedStorageDiff;

pub type SerializedAccessListEntry = AccessListItem;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedCodeDiff {
    pub hash: B256,
    pub bytecode: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub chunk_id: u32,
    pub total_chunks: u32,
    pub remaining_chunks_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMetrics {
    pub tx_count: usize,
    pub blob_count: usize,
    pub gas_used: u64,
    pub blob_gas_used: Option<u64>,
    pub block_value: U256,
    pub build_time_us: u64,
    pub trace: Option<SerializedBuildTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedBuildTrace {
    pub sim_time_us: u64,
    pub finalize_time_us: u64,
    pub root_hash_time_us: u64,
    pub ordering_time_us: u64,
    pub orders_considered: usize,
    pub orders_included: usize,
    pub orders_failed: usize,
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
    pub prev_randao: B256,
}

impl BlockParams {
    pub fn to_header_template(&self) -> Header {
        use alloy_consensus::constants::EMPTY_OMMER_ROOT_HASH;
        use alloy_eips::merge::BEACON_NONCE;
        use alloy_primitives::{FixedBytes, U256 as PrimU256};
        
        Header {
            parent_hash: self.parent_hash,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: self.coinbase,
            state_root: self.parent_state_root,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            difficulty: PrimU256::ZERO.into(),
            number: self.number,
            gas_limit: self.gas_limit.into(),
            gas_used: 0u64.into(),
            timestamp: self.timestamp,
            extra_data: alloy_primitives::Bytes::default(),
            mix_hash: self.prev_randao,
            nonce: FixedBytes::from(BEACON_NONCE.to_be_bytes()),
            base_fee_per_gas: Some(self.base_fee_per_gas.to::<u64>()),
            withdrawals_root: self.withdrawals_root,
            blob_gas_used: self.blob_gas_used.map(|v| v.into()),
            excess_blob_gas: self.excess_blob_gas.map(|v| v.into()),
            parent_beacon_block_root: self.parent_beacon_block_root,
            requests_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedAccount {
    pub address: Address,
    pub balance: U256,
    pub nonce: u64,
    pub code_hash: B256,
}

impl SerializedAccount {
    pub fn is_contract(&self) -> bool {
        self.code_hash != alloy_consensus::constants::KECCAK_EMPTY
    }

    pub fn is_empty(&self) -> bool {
        self.balance.is_zero() && self.nonce == 0 && self.code_hash == alloy_consensus::constants::KECCAK_EMPTY
    }
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
    pub gas_price: Option<U256>,
    pub nonce: u64,
    pub input: Bytes,
    pub tx_type: TxType,
    pub access_list: Vec<SerializedAccessListEntry>,
    pub blob_hashes: Vec<B256>,
    pub max_priority_fee_per_gas: Option<U256>,
    pub max_fee_per_gas: Option<U256>,
    pub max_fee_per_blob_gas: Option<U256>,
    pub versioned_hashes: Vec<B256>,
    pub encoded_signed_tx: Bytes,
}

impl SerializedTransaction {
    pub fn is_eip1559(&self) -> bool {
        matches!(self.tx_type, TxType::Eip1559 | TxType::Eip4844 | TxType::Eip7702)
    }

    pub fn is_blob_tx(&self) -> bool {
        self.tx_type == TxType::Eip4844
    }

    pub fn is_eip7702(&self) -> bool {
        self.tx_type == TxType::Eip7702
    }

    pub fn is_create(&self) -> bool {
        self.to.is_none()
    }

    pub fn effective_gas_price(&self, base_fee: U256) -> U256 {
        if let Some(gas_price) = self.gas_price {
            gas_price
        } else if let (Some(max_fee), Some(priority_fee)) = (self.max_fee_per_gas, self.max_priority_fee_per_gas) {
            (base_fee + priority_fee).min(max_fee)
        } else {
            base_fee
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedBundle {
    pub id: String,
    pub transactions: Vec<SerializedTransaction>,
    pub hash: B256,
    pub revertible: bool,
}

pub type SerializedWithdrawal = Withdrawal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderConfig {
    pub discard_txs: bool,
    pub sorting: SortingAlgorithm,
    pub failed_tx_retries: u32,
    pub drop_failed_txs: bool,
    pub coinbase_payment: bool,
    pub build_timeout_ms: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortingAlgorithm {
    GasPrice,
    Profit,
    MevGasPrice,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Legacy,
    OrderingOnlyMode,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        ExecutionMode::OrderingOnlyMode
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderingInput {
    pub block_number: u64,
    pub block_timestamp: u64,
    pub base_fee: U256,
    pub gas_limit: u64,
    pub orders: Vec<OrderForOrdering>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderForOrdering {
    pub id: String,
    pub order_type: String,
    pub coinbase_profit: U256,
    pub gas_used: u64,
    pub gas_price: U256,
    pub order_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SgxOrderingInput {
    pub block_number: u64,
    pub block_timestamp: u64,
    pub base_fee: U256,
    pub gas_limit: u64,
    pub orders: Vec<OrderForOrdering>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SgxOrderingResult {
    pub ordered_transaction_ids: Vec<String>,
    pub ordered_transactions: Vec<OrderedTransaction>,
    pub block_number: u64,
    pub timestamp: u64,
    pub sgx_signature: Option<Bytes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SgxOrderingOutput {
    pub ordered_transaction_ids: Vec<String>,
    pub block_number: u64,
    pub timestamp: u64,
    pub signature: Option<Bytes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderedTransaction {
    pub id: String,
    pub order_type: String,
    pub encoded_signed_tx: Bytes,
    pub order_id: String,
    pub order_hash: String,
}
