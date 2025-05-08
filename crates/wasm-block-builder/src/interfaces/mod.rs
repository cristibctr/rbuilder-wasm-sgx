
use crate::state::provider::WasiStateProvider;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

pub use input::{
    BlockBuilderConfig, BlockBuilderInput, BlockParams, SerializedAccount, SerializedBundle,
    SerializedCode, SerializedStorage, SerializedTransaction, StateProviderInput,
};
pub use output::{
    AccountDiff, BlockBuilderOutput, BlockMetrics, SerializedBuildTrace, SerializedHeader, SerializedLog,
    SerializedReceipt, SerializedStateDiff, SerializedAccountDiff, StorageDiff, SerializedStorageDiff,
    SerializedCodeDiff,
};

pub mod input;
pub mod output;

#[derive(Error, Debug, Clone)]
pub enum WasiError {
    #[error("Input Deserialization Error: {0}")]
    InputDeserialization(String),
    #[error("Output Serialization Error: {0}")]
    OutputSerialization(String),
    #[error("Block Builder Error: {0}")]
    Builder(String),
    #[error("EVM Error: {0}")]
    Evm(String),
    #[error("State Root Error: {0}")]
    StateRoot(String),
    #[error("State Provider Error: {0}")]
    State(String),
    #[error("Invalid Input Pointer")]
    NullInputPtr,
    #[error("Invalid Output Pointer")]
    NullOutputPtr,
    #[error("Output Buffer Too Small: required {required}, got {provided}")]
    OutputBufferTooSmall { required: usize, provided: usize },
    #[error("Internal Error: {0}")]
    Internal(String),
}

#[derive(Error, Debug, Clone)]
pub enum SerializationError {
    #[error("JSON serialization/deserialization failed: {0}")]
    Json(String),
    #[error("Invalid data format: {0}")]
    InvalidFormat(String),
}

impl From<serde_json::Error> for SerializationError {
    fn from(e: serde_json::Error) -> Self {
        SerializationError::Json(e.to_string())
    }
}

impl From<SerializationError> for WasiError {
    fn from(e: SerializationError) -> Self {
        match e {
            SerializationError::Json(s) => WasiError::InputDeserialization(s),
            SerializationError::InvalidFormat(s) => WasiError::InputDeserialization(s),
        }
    }
}


fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T, SerializationError> {
    serde_json::from_slice(data).map_err(|e| SerializationError::Json(e.to_string()))
}

fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>, SerializationError> {
    serde_json::to_vec(value).map_err(|e| SerializationError::Json(e.to_string()))
}


pub fn deserialize_block_input(data: &[u8]) -> Result<BlockBuilderInput, WasiError> {
    deserialize_json(data).map_err(WasiError::from)
}

pub fn deserialize_state_input(data: &[u8]) -> Result<StateProviderInput, WasiError> {
    deserialize_json(data).map_err(WasiError::from)
}

pub fn deserialize_state_changes(data: &[u8]) -> Result<SerializedStateDiff, WasiError> {
    deserialize_json(data).map_err(WasiError::from)
}

pub fn serialize_output(output: &BlockBuilderOutput) -> Result<Vec<u8>, WasiError> {
    serialize_json(output).map_err(|e| WasiError::OutputSerialization(e.to_string()))
}


pub fn serialize_output_without_signature(output: &BlockBuilderOutput) -> Result<Vec<u8>, WasiError> {
    let mut output_clone = output.clone();
    output_clone.signature = None;

    serialize_json(&output_clone).map_err(|e| WasiError::OutputSerialization(
        format!("Failed to serialize BlockBuilderOutput for signing: {}", e)
    ))
}

