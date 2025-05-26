mod inspector;
mod tracer;

use crate::interfaces::input::{BlockParams, SerializedTransaction};
use alloy_consensus::TxType;
use crate::state::provider::{StateError, WasiStateProvider};
use alloy_primitives::{Address, B256, Bytes, U256};
use log::{debug, error, info, warn};
use revm::{
    primitives::{
        AccessList, CreateScheme, ResultAndState as RevmResultAndState,
        TransactTo, TxEnv, EVMError as RevmEVMError, ExecutionResult, Output,
    },
    Evm, EvmContext,
};
use thiserror::Error;

pub use inspector::{WasiEVMInspector, UsedStateTrace, SlotKey};
pub use tracer::WasiExecutionTracer;

#[derive(Debug)]
pub struct ResultAndState {
    pub gas_used: u64,
    
    pub blob_gas_used: u64,
    
    pub success: bool,
    
    pub output: revm::primitives::Output,
    
    pub coinbase_profit: U256,
}

#[derive(Error, Debug)]
pub enum EVMError {
    #[error("EVM execution failed: {0}")]
    Execution(String),
    
    #[error("State provider error: {0}")]
    State(#[from] StateError),
    
    #[error("Gas estimation failed: {0}")]
    GasEstimation(String),

    #[error("REVM error: {0:?}")]
    Revm(RevmEVMError<StateError>),
    
    #[error("Conversion error: {0}")]
    Conversion(String),
}

impl From<RevmEVMError<StateError>> for EVMError {
    fn from(err: RevmEVMError<StateError>) -> Self {
        match err {
            RevmEVMError::Database(db_err) => EVMError::State(db_err),
            _ => EVMError::Revm(err),
        }
    }
}

fn configure_evm<'a>(
    state: &'a mut WasiStateProvider, 
    block_params: &'a BlockParams,
) -> revm::Evm<'a, (), &'a mut WasiStateProvider> {
    debug!("Configure EVM with parent_state_root: {:?}", block_params.parent_state_root);
    
    let mut evm = revm::Evm::builder()
        .with_db(state)
        .build();
    
    let mut block_env = revm::primitives::BlockEnv::default();
    block_env.number = U256::from(block_params.number);
    block_env.timestamp = U256::from(block_params.timestamp);
    block_env.coinbase = Address::from_slice(block_params.coinbase.as_slice());
    block_env.gas_limit = U256::from(block_params.gas_limit);
    block_env.basefee = block_params.base_fee_per_gas;
    block_env.prevrandao = Some(block_params.parent_hash);
    
    *evm.block_mut() = block_env;
    
    evm.cfg_mut().chain_id = 1;
    
    evm.cfg_mut().limit_contract_code_size = Some(0x100000);
    
    debug!("EVM configured with: number={}, timestamp={}, gas_limit={}, basefee={:?}", 
           block_params.number, block_params.timestamp, block_params.gas_limit, block_params.base_fee_per_gas);
    
    evm
}

fn to_revm_tx_env(tx: &SerializedTransaction, coinbase: Address) -> Result<TxEnv, EVMError> {
    let mut tx_env = TxEnv::default();
    
    tx_env.caller = tx.from;
    
    tx_env.value = tx.value;
    
    tx_env.data = tx.input.clone();
    
    tx_env.gas_limit = tx.gas_limit;
    
    match tx.tx_type {
        TxType::Legacy => {
            tx_env.gas_price = tx.gas_price.unwrap_or_default();
        }
        TxType::Eip2930 => {
            tx_env.gas_price = tx.gas_price.unwrap_or_default();
            tx_env.access_list = convert_access_list(&tx.access_list);
        }
        TxType::Eip1559 => {
            if let Some(priority_fee) = tx.max_priority_fee_per_gas {
                tx_env.gas_priority_fee = Some(priority_fee);
            }
            tx_env.gas_price = tx.gas_price.unwrap_or_default();
            tx_env.access_list = convert_access_list(&tx.access_list);
        }
        TxType::Eip4844 => {
            if let Some(priority_fee) = tx.max_priority_fee_per_gas {
                tx_env.gas_priority_fee = Some(priority_fee);
            }
            tx_env.gas_price = tx.gas_price.unwrap_or_default();
            tx_env.access_list = convert_access_list(&tx.access_list);
            
            if let Some(blob_fee) = tx.max_fee_per_blob_gas {
                tx_env.blob_hashes = tx.blob_hashes.clone();
                tx_env.max_fee_per_blob_gas = Some(blob_fee);
            }
        }
        TxType::Eip7702 => {
            if let Some(priority_fee) = tx.max_priority_fee_per_gas {
                tx_env.gas_priority_fee = Some(priority_fee);
            }
            tx_env.gas_price = tx.gas_price.unwrap_or_default();
            tx_env.access_list = convert_access_list(&tx.access_list);
        }
    }
    
    tx_env.transact_to = match tx.to {
        Some(to) => TransactTo::Call(to),
        None => TransactTo::Create,
    };
    
    tx_env.nonce = Some(tx.nonce);
    
    Ok(tx_env)
}

fn convert_access_list(access_list: &[crate::interfaces::input::SerializedAccessListEntry]) -> Vec<revm::primitives::AccessListItem> {
    let mut revm_access_list = Vec::new();
    for entry in access_list {
        revm_access_list.push(revm::primitives::AccessListItem {
            address: entry.address,
            storage_keys: entry.storage_keys.clone(),
        });
    }
    revm_access_list
}

pub fn estimate_gas(
    tx: &SerializedTransaction,
    state: &WasiStateProvider,
) -> Result<u64, EVMError> {
    let mut state_copy = state.clone();
    
    let block_params = BlockParams {
        number: 0,
        timestamp: 0,
        gas_limit: 30_000_000,
        base_fee_per_gas: U256::from(1_000_000_000),
        coinbase: Address::ZERO,
        parent_hash: B256::ZERO,
        parent_state_root: B256::ZERO,
        withdrawals_root: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
        prev_randao: B256::ZERO,
    };
    
    let mut evm = configure_evm(&mut state_copy, &block_params);
    
    let mut tx_env = to_revm_tx_env(tx, block_params.coinbase)?;
    
    let mut low = 21_000;
    let mut high = 30_000_000;
    
    while low < high {
        let mid = (low + high) / 2;
        tx_env.gas_limit = mid;
        
        *evm.tx_mut() = tx_env.clone();
        
        let res = evm.transact().map_err(|e| EVMError::from(e))?;
        
        if matches!(res.result, ExecutionResult::Success { .. }) {
            high = mid;
        } else {
            if matches!(res.result, ExecutionResult::Revert { .. } | ExecutionResult::Halt { reason: revm::primitives::HaltReason::OutOfGas(_), .. }) {
                low = mid + 1;
            } else {
                return Ok(mid);
            }
        }
    }
    
    let estimated_gas = (high * 105) / 100;
    Ok(estimated_gas)
}

pub fn execute_transaction(
    tx: &SerializedTransaction,
    state: &mut WasiStateProvider,
    block_params: &BlockParams,
) -> Result<ResultAndState, EVMError> {
    let mut evm = configure_evm(state, block_params);
    
    let tx_env = to_revm_tx_env(tx, block_params.coinbase)?;
    *evm.tx_mut() = tx_env;
    
    let result = evm.transact_commit().map_err(|e| EVMError::from(e))?;
    
    if let ExecutionResult::Success { gas_used, output, .. } = result {
        let blob_gas_used = match tx.tx_type {
            TxType::Eip4844 => tx.blob_hashes.len() as u64 * 131072,
            _ => 0,
        };
        
        let output_bytes = output;
        
        let gas_price = if let Some(priority_fee) = tx.max_priority_fee_per_gas {
            let base_plus_priority = block_params.base_fee_per_gas + priority_fee;
            let tx_gas_price = tx.gas_price.unwrap_or_default();
            if base_plus_priority < tx_gas_price {
                base_plus_priority
            } else {
                tx_gas_price
            }
        } else {
            tx.gas_price.unwrap_or_default()
        };
        
        let coinbase_profit = gas_price * U256::from(gas_used);
        
        Ok(ResultAndState {
            gas_used,
            blob_gas_used,
            success: true,
            output: output_bytes,
            coinbase_profit,
        })
    } else {
        Err(EVMError::Execution(format!("Transaction execution failed: {:?}", result)))
    }
}

pub fn execute_transaction_with_trace(
    tx: &SerializedTransaction,
    state: &mut WasiStateProvider,
    block_params: &BlockParams,
    state_trace: &mut UsedStateTrace,
) -> Result<ResultAndState, EVMError> {
    let mut inspector = WasiEVMInspector::new(state_trace);
    
    debug!("Executing transaction with trace: hash={:?}, from={:?}, to={:?}, nonce={}, value={:?}", 
           tx.hash, tx.from, tx.to, tx.nonce, tx.value);
    
    inspector.track_tx_nonce(tx.from, tx.nonce);
    
    let mut state_clone = state.clone();
    
    let mut evm = configure_evm(&mut state_clone, block_params);
    
    let tx_env = to_revm_tx_env(tx, block_params.coinbase)?;
    *evm.tx_mut() = tx_env;
    
    let mut evm_with_inspector = revm::Evm::builder()
        .with_db(state)
        .with_external_context(inspector)
        .append_handler_register(revm::inspector_handle_register)
        .with_env(Box::new(*evm.context.evm.env.clone()))
        .build();
    
    let result = evm_with_inspector.transact_commit().map_err(|e| {
        debug!("Transaction execution error: {:?}, hash={:?}, from={:?}, to={:?}", 
               e, tx.hash, tx.from, tx.to);
        EVMError::from(e)
    })?;
    
    if let ExecutionResult::Success { gas_used, output, .. } = result {
        let blob_gas_used = match tx.tx_type {
            TxType::Eip4844 => tx.blob_hashes.len() as u64 * 131072,
            _ => 0,
        };
        
        let output_bytes = output;
        
        let gas_price = if let Some(priority_fee) = tx.max_priority_fee_per_gas {
            let base_plus_priority = block_params.base_fee_per_gas + priority_fee;
            let tx_gas_price = tx.gas_price.unwrap_or_default();
            if base_plus_priority < tx_gas_price {
                base_plus_priority
            } else {
                tx_gas_price
            }
        } else {
            tx.gas_price.unwrap_or_default()
        };
        
        let coinbase_profit = gas_price * U256::from(gas_used);
        
        debug!("Transaction executed successfully: hash={:?}, gas_used={}, value={:?}, profit={:?}", 
               tx.hash, gas_used, tx.value, coinbase_profit);
        
        Ok(ResultAndState {
            gas_used,
            blob_gas_used,
            success: true,
            output: output_bytes,
            coinbase_profit,
        })
    } else {
        let error_msg = match &result {
            ExecutionResult::Revert { gas_used, output } => {
                format!("Transaction reverted with output: {:?}, gas_used: {}", output, gas_used)
            },
            ExecutionResult::Halt { reason, gas_used } => {
                format!("Transaction halted: {:?}, gas_used: {}", reason, gas_used)
            },
            _ => format!("Transaction execution failed: {:?}", result)
        };
        
        debug!("Transaction execution failed: hash={:?}, from={:?}, to={:?}, value={:?}, gas_limit={}, error={}", 
               tx.hash, tx.from, tx.to, tx.value, tx.gas_limit, error_msg);
        
        Err(EVMError::Execution(error_msg))
    }
}
