pub use block_builder_types::*;

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

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
pub struct StateProviderInput {
    pub accounts: Vec<SerializedAccount>,
    
    pub storage: Vec<SerializedStorage>,
    
    pub code: Vec<SerializedCode>,
}
