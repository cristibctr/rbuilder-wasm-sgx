use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde::de::{Error, Visitor};
    use std::fmt;
    use std::marker::PhantomData;

    pub fn serialize<S, const N: usize>(bytes: &[u8; N], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            let hex = hex::encode(bytes);
            serializer.serialize_str(&hex)
        } else {
            bytes.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D, const N: usize>(deserializer: D) -> Result<[u8; N], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor<const N: usize>(PhantomData<[u8; N]>);

        impl<'de, const N: usize> Visitor<'de> for BytesVisitor<N> {
            type Value = [u8; N];

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "a byte array of length {}", N)
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                let bytes = hex::decode(v)
                    .map_err(|_| Error::custom("invalid hex string"))?;
                if bytes.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        bytes.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(&bytes);
                Ok(result)
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v.len() != N {
                    return Err(Error::custom(format!(
                        "expected {} bytes, got {}",
                        N,
                        v.len()
                    )));
                }
                let mut result = [0u8; N];
                result.copy_from_slice(v);
                Ok(result)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut result = [0u8; N];
                for i in 0..N {
                    match seq.next_element()? {
                        Some(v) => result[i] = v,
                        None => return Err(Error::custom(format!(
                            "expected {} bytes, got {}",
                            N, i
                        ))),
                    }
                }
                Ok(result)
            }
        }

        if deserializer.is_human_readable() {
            deserializer.deserialize_str(BytesVisitor(PhantomData))
        } else {
            deserializer.deserialize_bytes(BytesVisitor(PhantomData))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderOutput {
    pub header: SerializedHeader,
    
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
pub struct SerializedHeader {
    pub parent_hash: B256,
    
    pub number: u64,
    
    pub timestamp: u64,
    
    pub coinbase: Address,
    
    pub difficulty: U256,
    
    pub gas_limit: u64,
    
    pub gas_used: u64,
    
    pub base_fee_per_gas: U256,
    
    pub extra_data: Bytes,
    
    pub state_root: B256,
    
    pub transactions_root: B256,
    
    pub receipts_root: B256,
    
    #[serde(with = "serde_bytes_array")]
    pub logs_bloom: [u8; 256],
    
    pub mix_hash: B256,
    
    pub withdrawals_root: Option<B256>,
    
    pub blob_gas_used: Option<u64>,
    
    pub excess_blob_gas: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedReceipt {
    pub tx_type: u8,
    
    pub success: bool,
    
    pub cumulative_gas_used: u64,
    
    pub logs: Vec<SerializedLog>,
    
    #[serde(with = "serde_bytes_array")]
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