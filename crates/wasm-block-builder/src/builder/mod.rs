mod ordering;
mod simulator;

use crate::{interfaces::{
    input::{BlockBuilderConfig, BlockParams, SerializedBundle, SerializedTransaction},
    output::{BlockBuilderOutput, BlockMetrics, SerializedBuildTrace},
}, sgx_log, state::WasiStateProvider};
use alloy_consensus::{Header, TxReceipt};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_consensus::proofs::{calculate_transaction_root, calculate_receipt_root};
use alloy_eips::eip2718::{Typed2718, Encodable2718};
use alloy_rlp::{BufMut, Encodable};
use std::time::Instant;
use thiserror::Error;
use crate::state::CompressionLevel;
use self::ordering::WasiOrderSorter;

pub use self::simulator::WasiSimulator;

#[derive(Clone)]
struct TxWrapper(Bytes);



impl alloy_eips::eip2718::Typed2718 for TxWrapper {
    fn ty(&self) -> u8 {
        if self.0.is_empty() {
            0
        } else {
            match self.0[0] {
                1 | 2 | 3 => self.0[0],
                _ => 0,
            }
        }
    }
}

impl alloy_eips::eip2718::Encodable2718 for TxWrapper {
    fn type_flag(&self) -> Option<u8> {
        match self.ty() {
            0 => None,
            ty => Some(ty),
        }
    }
    
    fn encode_2718_len(&self) -> usize {
        self.0.len()
    }
    
    fn encode_2718(&self, out: &mut dyn alloy_rlp::BufMut) {
        out.put_slice(&self.0);
    }
}

#[derive(Error, Debug)]
pub enum BlockBuilderError {
    #[error("Simulation failed: {0}")]
    Simulation(String),
    
    #[error("Block building failed: {0}")]
    Building(String),
    
    #[error("State error: {0}")]
    State(#[from] crate::state::provider::StateError),
    
    #[error("EVM error: {0}")]
    EVM(#[from] crate::evm::EVMError),
    
    #[error("Timeout: Block building exceeded time limit")]
    Timeout,
}

pub struct WasiBlockBuilder {
    state: WasiStateProvider,
    
    block_params: BlockParams,
    
    config: BlockBuilderConfig,
    
    sorter: WasiOrderSorter,
    
    simulator: WasiSimulator,
}

impl WasiBlockBuilder {
    pub fn new(
        state: WasiStateProvider,
        block_params: BlockParams,
        config: BlockBuilderConfig,
    ) -> Self {
        let sorting_algo = config.sorting;
        let block_params_clone = block_params.clone();
        Self {
            state,
            block_params,
            config,
            sorter: WasiOrderSorter::new(sorting_algo).with_block_params(block_params_clone),
            simulator: WasiSimulator::new(),
        }
    }
    
    pub fn simulator(&self) -> &WasiSimulator {
        &self.simulator
    }
    
    pub fn build_block(
        &mut self,
        transactions: Vec<SerializedTransaction>,
        bundles: Vec<SerializedBundle>,
    ) -> Result<BlockBuilderOutput, BlockBuilderError> {
        let build_start = Instant::now();
        
        let orders_considered = transactions.len() + bundles.len();
        let error_msg = format!("Sorting {} transactions and {} bundles", transactions.len(), bundles.len());
        log::info!("{}", error_msg);
        sgx_log(&error_msg);

        
        let ordering_start = Instant::now();
        let ordered_txs = self.sorter.sort_transactions(
            transactions, 
            bundles,
            &mut self.state,
        )?;
        let ordering_time = ordering_start.elapsed();
        
        let error_msg = format!("Simulating transactions for block building");
        log::info!("{}", error_msg);
        sgx_log(&error_msg);

        let sim_start = Instant::now();
        let (included_txs, receipts) = self.simulator.simulate_and_build(
            &mut self.state,
            ordered_txs,
            &self.block_params,
            &self.config,
        )?;
        let sim_time = sim_start.elapsed();
        
        let root_hash_start = Instant::now();
        
        let compression_level = match self.config.compression_level.to_lowercase().as_str() {
            "none" => CompressionLevel::None,
            "low" => CompressionLevel::Low,
            "medium" => CompressionLevel::Medium,
            "high" => CompressionLevel::High,
            _ => CompressionLevel::Medium,
        };
        
        let (state_diff, enhanced_diff, chunk_info, build_id) = self.simulator.calculate_state_diff(
            &self.state,
            self.config.complete_state_diff,
            self.config.include_merkle_proofs,
            compression_level
        )?;
        
        let state_root: Option<B256> = None;
        let root_hash_time = root_hash_start.elapsed();
        
        let finalize_start = Instant::now();
        
        use alloy_primitives::{ChainId};
        use alloy_rlp::Encodable;
        
        let tx_bytes: Vec<Bytes> = included_txs
            .iter()
            .map(|tx| {
                tx.encoded_signed_tx.clone()
            })
            .collect();
            
        let transactions_root = if tx_bytes.is_empty() {
            B256::ZERO
        } else {
            let tx_wrappers: Vec<_> = tx_bytes.iter().map(|bytes| TxWrapper(bytes.clone())).collect();
            calculate_transaction_root(&tx_wrappers)
        };
        
        let receipts_root = if receipts.is_empty() {
            B256::ZERO
        } else {
            let receipts_with_bloom: Vec<_> = receipts.iter().map(|receipt| {
                receipt.with_bloom_ref()
            }).collect();
            calculate_receipt_root(&receipts_with_bloom)
        };
        
        let logs_bloom = self.calculate_logs_bloom(&receipts);
        
        let state_root = crate::state::reth_compatible_root::calculate_state_root(&state_diff, &self.state)
            .map_err(|e| BlockBuilderError::Building(format!("Failed to calculate state root: {}", e)))?;
        
        let finalize_time = finalize_start.elapsed();
        
        let metrics = BlockMetrics {
            tx_count: included_txs.len(),
            blob_count: included_txs.iter().map(|tx| tx.blob_hashes.len()).sum(),
            gas_used: self.simulator.gas_used(),
            blob_gas_used: Some(self.simulator.blob_gas_used()),
            block_value: self.simulator.coinbase_profit(),
            build_time_us: build_start.elapsed().as_micros() as u64,
            trace: Some(SerializedBuildTrace {
                sim_time_us: sim_time.as_micros() as u64,
                finalize_time_us: finalize_time.as_micros() as u64,
                root_hash_time_us: root_hash_time.as_micros() as u64,
                ordering_time_us: ordering_time.as_micros() as u64,
                orders_considered,
                orders_included: included_txs.len(),
                orders_failed: self.simulator.failed_tx_count(),
            }),
        };
        
        let header = self.create_header(metrics.gas_used, transactions_root, receipts_root, state_root, logs_bloom)?;
        
        Ok(BlockBuilderOutput {
            header,
            transactions: tx_bytes,
            receipts,
            state_diff,
            state_root: Some(state_root),
            metrics,
            signature: None,
            chunk_info,
            build_id,
            execution_requests: None,
        })
    }
    
    fn create_header(&self, gas_used: u64, transactions_root: B256, receipts_root: B256, state_root: B256, logs_bloom: [u8; 256]) -> Result<Header, BlockBuilderError> {
        let mut header = self.block_params.to_header_template();
        
        header.state_root = state_root;
        header.transactions_root = transactions_root;
        header.receipts_root = receipts_root;
        header.logs_bloom = alloy_primitives::Bloom::from_slice(&logs_bloom);
        header.gas_used = gas_used.into();
        
        Ok(header)
    }
    

    fn calculate_logs_bloom(&self, receipts: &[crate::interfaces::output::SerializedReceipt]) -> [u8; 256] {
        use alloy_primitives::{Log, LogData, Bloom};
        
        let logs: Vec<Log> = receipts
            .iter()
            .flat_map(|receipt| &receipt.logs)
            .map(|log| {
                Log {
                    address: log.address,
                    data: LogData::new_unchecked(log.topics().to_vec(), log.data.data.clone()),
                }
            })
            .collect();
            
        let bloom = alloy_primitives::logs_bloom(logs.iter());
        *bloom.data()
    }
}