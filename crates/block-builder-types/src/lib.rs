use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_consensus::Header;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedReceipt {
    pub tx_type: u8,
    pub success: bool,
    pub cumulative_gas_used: u64,
    pub logs: Vec<SerializedLog>,
    #[serde(with = "serde_utils::serde_bytes_array")]
    pub logs_bloom: [u8; 256],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
}

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

#[cfg(feature = "encoding")]
impl Typed2718 for SerializedReceipt {
    fn ty(&self) -> u8 {
        self.tx_type
    }
}

#[cfg(feature = "encoding")]
impl Encodable2718 for SerializedReceipt {
    fn type_flag(&self) -> Option<u8> {
        match self.tx_type {
            0 => None,
            ty => Some(ty),
        }
    }

    fn encode_2718_len(&self) -> usize {
        let payload_len = self.encode_fields_len();
        if self.tx_type == 0 {
            payload_len
        } else {
            1 + payload_len
        }
    }

    fn encode_2718(&self, out: &mut dyn BufMut) {
        if self.tx_type != 0 {
            out.put_u8(self.tx_type);
        }
        self.encode_fields(out);
    }
}

#[cfg(feature = "encoding")]
impl SerializedReceipt {
    fn encode_fields_len(&self) -> usize {
        let status = if self.success { 1u8 } else { 0u8 };
        status.length() + 
        self.cumulative_gas_used.length() +
        self.logs_bloom.length() +
        self.logs.length()
    }
    
    fn encode_fields(&self, out: &mut dyn BufMut) {
        let status = if self.success { 1u8 } else { 0u8 };
        status.encode(out);
        self.cumulative_gas_used.encode(out);
        self.logs_bloom.encode(out);
        self.logs.encode(out);
    }
}

#[cfg(feature = "encoding")]
impl Encodable for SerializedLog {
    fn encode(&self, out: &mut dyn BufMut) {
        self.address.encode(out);
        self.topics.encode(out);
        self.data.encode(out);
    }
    
    fn length(&self) -> usize {
        self.address.length() + self.topics.length() + self.data.length()
    }
}