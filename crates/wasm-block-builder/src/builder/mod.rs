mod ordering;
mod simulator;

use crate::{
    interfaces::{
        input::{BlockBuilderConfig, BlockParams, SerializedBundle, SerializedTransaction},
        output::{BlockBuilderOutput, BlockMetrics, SerializedBuildTrace},
    },
    state::WasiStateProvider,
};
use alloy_primitives::{Address, B256, Bytes, U256};
use std::time::Instant;
use thiserror::Error;

use self::ordering::WasiOrderSorter;

pub use self::simulator::WasiSimulator;

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
    
    pub fn build_block(
        &mut self,
        transactions: Vec<SerializedTransaction>,
        bundles: Vec<SerializedBundle>,
    ) -> Result<BlockBuilderOutput, BlockBuilderError> {
        let build_start = Instant::now();
        
        let orders_considered = transactions.len() + bundles.len();
        log::info!("Sorting {} transactions and {} bundles", transactions.len(), bundles.len());
        
        let ordering_start = Instant::now();
        let ordered_txs = self.sorter.sort_transactions(
            transactions, 
            bundles,
            &mut self.state,
        )?;
        let ordering_time = ordering_start.elapsed();
        
        log::info!("Simulating transactions for block building");
        let sim_start = Instant::now();
        let (included_txs, receipts) = self.simulator.simulate_and_build(
            &mut self.state,
            ordered_txs,
            &self.block_params,
            &self.config,
        )?;
        let sim_time = sim_start.elapsed();
        
        let root_hash_start = Instant::now();
        let state_diff = self.simulator.calculate_state_diff(&self.state)?;
        
        let state_root = None;
        let root_hash_time = root_hash_start.elapsed();
        
        let finalize_start = Instant::now();
        
        use alloy_primitives::{ChainId};
        use alloy_rlp::Encodable;
        
        let tx_bytes: Vec<Bytes> = included_txs
            .iter()
            .map(|tx| {
                let chain_id = 1u64;
                
                match tx.tx_type {
                    crate::interfaces::input::TxType::Legacy => {
                        let mut buffer = Vec::new();
                        
                        let r = tx.hash;
                        let s = B256::from_slice(&tx.hash.0[0..32]);
                        
                        let v = 27u64 + (chain_id * 2) + 35;
                        
                        let mut data = Vec::new();
                        
                        tx.nonce.encode(&mut data);
                        tx.gas_price.encode(&mut data);
                        tx.gas_limit.encode(&mut data);
                        match tx.to {
                            Some(to) => to.encode(&mut data),
                            None => Bytes::new().encode(&mut data),
                        }
                        tx.value.encode(&mut data);
                        tx.input.as_ref().encode(&mut data);
                        v.encode(&mut data);
                        r.encode(&mut data);
                        s.encode(&mut data);
                        
                        let list_header = alloy_rlp::Header {
                            list: true,
                            payload_length: data.len()
                        };
                        list_header.encode(&mut buffer);
                        buffer.extend_from_slice(&data);
                        
                        Bytes::from(buffer)
                    },
                    
                    crate::interfaces::input::TxType::AccessList => {
                        let mut buffer = Vec::new();
                        buffer.push(tx.tx_type as u8);
                        
                        let r = tx.hash;
                        let s = B256::from_slice(&tx.hash.0[0..32]);
                        let v = 1u64;
                        
                        let mut rlp_data = Vec::new();
                        
                        chain_id.encode(&mut rlp_data);
                        tx.nonce.encode(&mut rlp_data);
                        tx.gas_price.encode(&mut rlp_data);
                        tx.gas_limit.encode(&mut rlp_data);
                        
                        match tx.to {
                            Some(to) => to.encode(&mut rlp_data),
                            None => Bytes::new().encode(&mut rlp_data),
                        }
                        
                        tx.value.encode(&mut rlp_data);
                        tx.input.as_ref().encode(&mut rlp_data);
                        
                        let mut access_list_rlp = Vec::new();
                        for entry in &tx.access_list {
                            let mut entry_rlp: Vec<u8> = Vec::new();
                            

                            let mut entry_buffer = Vec::new();
                            
                            let header = alloy_rlp::Header { list: true, payload_length: 0 };
                            
                            entry.address.encode(&mut entry_buffer);
                            
                            let slots_header = alloy_rlp::Header { list: true, payload_length: 0 };
                            let mut slots_buffer = Vec::new();
                            
                            for slot in &entry.slots {
                                slot.encode(&mut slots_buffer);
                            }
                            
                            let slots_header = alloy_rlp::Header {
                                list: true, 
                                payload_length: slots_buffer.len() 
                            };
                            slots_header.encode(&mut entry_buffer);
                            entry_buffer.extend_from_slice(&slots_buffer);
                            
                            let entry_header = alloy_rlp::Header {
                                list: true,
                                payload_length: entry_buffer.len()
                            };
                            entry_header.encode(&mut access_list_rlp);
                            access_list_rlp.extend_from_slice(&entry_buffer);
                        }
                        
                        let access_list_header = alloy_rlp::Header {
                            list: true,
                            payload_length: access_list_rlp.len()
                        };
                        access_list_header.encode(&mut rlp_data);
                        rlp_data.extend_from_slice(&access_list_rlp);
                        
                        v.encode(&mut rlp_data);
                        r.encode(&mut rlp_data);
                        s.encode(&mut rlp_data);
                        
                        let mut final_buffer = Vec::new();
                        alloy_rlp::Header { list: true, payload_length: rlp_data.len() }.encode(&mut final_buffer);
                        final_buffer.extend_from_slice(&rlp_data);
                        
                        buffer.extend_from_slice(&final_buffer);
                        Bytes::from(buffer)
                    },
                    
                    crate::interfaces::input::TxType::EIP1559 => {
                        let mut buffer = Vec::new();
                        buffer.push(tx.tx_type as u8);
                        
                        let max_priority_fee = tx.max_priority_fee_per_gas.unwrap_or(U256::ZERO);
                        
                        let r = tx.hash;
                        let s = B256::from_slice(&tx.hash.0[0..32]);
                        let v = 1u64;
                        
                        let mut rlp_data = Vec::new();
                        
                        chain_id.encode(&mut rlp_data);
                        tx.nonce.encode(&mut rlp_data);
                        max_priority_fee.encode(&mut rlp_data);
                        tx.gas_price.encode(&mut rlp_data);
                        tx.gas_limit.encode(&mut rlp_data);
                        
                        match tx.to {
                            Some(to) => to.encode(&mut rlp_data),
                            None => Bytes::new().encode(&mut rlp_data),
                        }
                        
                        tx.value.encode(&mut rlp_data);
                        tx.input.as_ref().encode(&mut rlp_data);
                        
                        let mut access_list_rlp = Vec::new();
                        for entry in &tx.access_list {
                            let mut entry_rlp: Vec<u8> = Vec::new();
                            

                            let mut entry_buffer = Vec::new();
                            
                            let header = alloy_rlp::Header { list: true, payload_length: 0 };
                            
                            entry.address.encode(&mut entry_buffer);
                            
                            let slots_header = alloy_rlp::Header { list: true, payload_length: 0 };
                            let mut slots_buffer = Vec::new();
                            
                            for slot in &entry.slots {
                                slot.encode(&mut slots_buffer);
                            }
                            
                            let slots_header = alloy_rlp::Header {
                                list: true, 
                                payload_length: slots_buffer.len() 
                            };
                            slots_header.encode(&mut entry_buffer);
                            entry_buffer.extend_from_slice(&slots_buffer);
                            
                            let entry_header = alloy_rlp::Header {
                                list: true,
                                payload_length: entry_buffer.len()
                            };
                            entry_header.encode(&mut access_list_rlp);
                            access_list_rlp.extend_from_slice(&entry_buffer);
                        }
                        
                        let access_list_header = alloy_rlp::Header {
                            list: true,
                            payload_length: access_list_rlp.len()
                        };
                        access_list_header.encode(&mut rlp_data);
                        rlp_data.extend_from_slice(&access_list_rlp);
                        
                        v.encode(&mut rlp_data);
                        r.encode(&mut rlp_data);
                        s.encode(&mut rlp_data);
                        
                        let mut final_buffer = Vec::new();
                        alloy_rlp::Header { list: true, payload_length: rlp_data.len() }.encode(&mut final_buffer);
                        final_buffer.extend_from_slice(&rlp_data);
                        
                        buffer.extend_from_slice(&final_buffer);
                        Bytes::from(buffer)
                    },
                    
                    crate::interfaces::input::TxType::Blob => {
                        let mut buffer = Vec::new();
                        buffer.push(tx.tx_type as u8);
                        
                        let max_priority_fee = tx.max_priority_fee_per_gas.unwrap_or(U256::ZERO);
                        
                        let max_blob_fee = tx.max_fee_per_blob_gas.unwrap_or(U256::ZERO);
                        
                        let r = tx.hash;
                        let s = B256::from_slice(&tx.hash.0[0..32]);
                        let v = 1u64;
                        
                        let mut rlp_data = Vec::new();
                        
                        chain_id.encode(&mut rlp_data);
                        tx.nonce.encode(&mut rlp_data);
                        max_priority_fee.encode(&mut rlp_data);
                        tx.gas_price.encode(&mut rlp_data);
                        tx.gas_limit.encode(&mut rlp_data);
                        
                        match tx.to {
                            Some(to) => to.encode(&mut rlp_data),
                            None => Bytes::new().encode(&mut rlp_data),
                        }
                        
                        tx.value.encode(&mut rlp_data);
                        tx.input.as_ref().encode(&mut rlp_data);
                        
                        let mut access_list_rlp = Vec::new();
                        for entry in &tx.access_list {
                            let mut entry_rlp: Vec<u8> = Vec::new();
                            
                            
                            let mut entry_buffer = Vec::new();
                            
                            let header = alloy_rlp::Header { list: true, payload_length: 0 };
                            
                            entry.address.encode(&mut entry_buffer);
                            
                            let slots_header = alloy_rlp::Header { list: true, payload_length: 0 };
                            let mut slots_buffer = Vec::new();
                            
                            for slot in &entry.slots {
                                slot.encode(&mut slots_buffer);
                            }
                            
                            let slots_header = alloy_rlp::Header { 
                                list: true, 
                                payload_length: slots_buffer.len() 
                            };
                            slots_header.encode(&mut entry_buffer);
                            entry_buffer.extend_from_slice(&slots_buffer);
                            
                            let entry_header = alloy_rlp::Header {
                                list: true,
                                payload_length: entry_buffer.len()
                            };
                            entry_header.encode(&mut access_list_rlp);
                            access_list_rlp.extend_from_slice(&entry_buffer);
                        }
                        
                        let access_list_header = alloy_rlp::Header {
                            list: true,
                            payload_length: access_list_rlp.len()
                        };
                        access_list_header.encode(&mut rlp_data);
                        rlp_data.extend_from_slice(&access_list_rlp);
                        
                        max_blob_fee.encode(&mut rlp_data);
                        
                        let mut blob_hashes_buffer = Vec::new();
                        
                        for hash in &tx.versioned_hashes {
                            hash.encode(&mut blob_hashes_buffer);
                        }
                        
                        let blob_header = alloy_rlp::Header {
                            list: true,
                            payload_length: blob_hashes_buffer.len()
                        };
                        blob_header.encode(&mut rlp_data);
                        rlp_data.extend_from_slice(&blob_hashes_buffer);
                        
                        v.encode(&mut rlp_data);
                        r.encode(&mut rlp_data);
                        s.encode(&mut rlp_data);
                        
                        let mut final_buffer = Vec::new();
                        alloy_rlp::Header { list: true, payload_length: rlp_data.len() }.encode(&mut final_buffer);
                        final_buffer.extend_from_slice(&rlp_data);
                        
                        buffer.extend_from_slice(&final_buffer);
                        Bytes::from(buffer)
                    }
                }
            })
            .collect();
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
        
        let header = self.create_header(metrics.gas_used)?;
        
        Ok(BlockBuilderOutput {
            header,
            transactions: tx_bytes,
            receipts,
            state_diff,
            state_root,
            metrics,
            signature: None,
        })
    }
    
    fn create_header(&self, gas_used: u64) -> Result<crate::interfaces::output::SerializedHeader, BlockBuilderError> {
        Ok(crate::interfaces::output::SerializedHeader {
            parent_hash: self.block_params.parent_hash,
            number: self.block_params.number,
            timestamp: self.block_params.timestamp,
            coinbase: self.block_params.coinbase,
            difficulty: U256::ZERO,
            gas_limit: self.block_params.gas_limit,
            gas_used,
            base_fee_per_gas: self.block_params.base_fee_per_gas,
            extra_data: Bytes::default(),
            state_root: B256::ZERO,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: [0u8; 256],
            mix_hash: B256::ZERO,
            withdrawals_root: self.block_params.withdrawals_root,
            blob_gas_used: self.block_params.blob_gas_used,
            excess_blob_gas: self.block_params.excess_blob_gas,
        })
    }
}