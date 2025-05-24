use crate::{
    evm,
    interfaces::input::{BlockParams, SerializedBundle, SerializedTransaction, SortingAlgorithm},
    state::WasiStateProvider,
};
use alloy_primitives::{Address, B256, U256};
use super::BlockBuilderError;
use log::{debug, info};

#[derive(Debug, Clone)]
pub struct OrderedTransaction {
    pub transaction: SerializedTransaction,
    
    pub priority: OrderPriority,
    
    pub in_bundle: bool,
    
    pub bundle_hash: Option<B256>,
    
    pub simulated_gas_used: Option<u64>,
    
    pub simulated_profit: Option<U256>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrderPriority {
    GasPrice(U256),
    
    Profit(U256),
    
    MevGasPrice(U256),
    
    BundleLength(usize, U256),
    
    BundleFirst(bool, U256),
}

pub struct WasiOrderSorter {
    algorithm: SortingAlgorithm,
    
    block_params: BlockParams,
}

impl WasiOrderSorter {
    pub fn new(algorithm: SortingAlgorithm) -> Self {
        let block_params = BlockParams {
            number: 1,
            timestamp: 1,
            gas_limit: 30_000_000,
            base_fee_per_gas: U256::from(1_000_000_000),
            coinbase: Address::ZERO,
            parent_hash: B256::ZERO,
            parent_state_root: B256::ZERO,
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        };
        
        Self { 
            algorithm,
            block_params,
        }
    }
    
    pub fn with_block_params(mut self, block_params: BlockParams) -> Self {
        self.block_params = block_params;
        self
    }
    
    pub fn sort_transactions(
        &self,
        transactions: Vec<SerializedTransaction>,
        bundles: Vec<SerializedBundle>,
        state: &mut WasiStateProvider,
    ) -> Result<Vec<OrderedTransaction>, BlockBuilderError> {
        let mut ordered_txs = Vec::new();
        
        for tx in transactions {
            let (priority, gas_used, profit) = self.calculate_priority(&tx, state)?;
            ordered_txs.push(OrderedTransaction {
                transaction: tx,
                priority,
                in_bundle: false,
                bundle_hash: None,
                simulated_gas_used: gas_used,
                simulated_profit: profit,
            });
        }
        
        for bundle in bundles {
            let bundle_length = bundle.transactions.len();
            let is_bundle = true;
            
            let mut total_profit = U256::ZERO;
            let mut any_failed = false;
            
            for tx in &bundle.transactions {
                let (_, _, profit) = self.calculate_priority(tx, state)?;
                if let Some(p) = profit {
                    total_profit += p;
                } else {
                    any_failed = true;
                    break;
                }
            }
            
            if any_failed && !bundle.revertible {
                debug!("Skipping bundle that failed in simulation: {:?}", bundle.hash);
                continue;
            }
            
            for tx in bundle.transactions {
                let (mut priority, gas_used, profit) = self.calculate_priority(&tx, state)?;
                
                if self.algorithm == SortingAlgorithm::Profit {
                    priority = OrderPriority::BundleFirst(is_bundle, total_profit);
                }
                
                ordered_txs.push(OrderedTransaction {
                    transaction: tx,
                    priority,
                    in_bundle: true,
                    bundle_hash: Some(bundle.hash),
                    simulated_gas_used: gas_used,
                    simulated_profit: profit,
                });
            }
        }
        
        ordered_txs.sort_by(|a, b| b.priority.cmp(&a.priority));
        
        Ok(ordered_txs)
    }
    
    fn calculate_priority(
        &self, 
        tx: &SerializedTransaction,
        state: &mut WasiStateProvider,
    ) -> Result<(OrderPriority, Option<u64>, Option<U256>), BlockBuilderError> {
        let gas_price = match tx.max_priority_fee_per_gas {
            Some(priority_fee) => {
                self.block_params.base_fee_per_gas + priority_fee
            }
            None => {
                tx.gas_price
            }
        };
        
        match self.algorithm {
            SortingAlgorithm::GasPrice => {
                Ok((OrderPriority::GasPrice(gas_price), None, None))
            }
            SortingAlgorithm::Profit | SortingAlgorithm::MevGasPrice => {
                info!("Simulating transaction for profit calculation: {:?}", tx.hash);
                
                let mut state_copy = state.clone();
                
                match evm::execute_transaction(tx, &mut state_copy, &self.block_params) {
                    Ok(result) => {
                        let gas_used = result.gas_used;
                        let profit = result.coinbase_profit;
                        
                        let priority = match self.algorithm {
                            SortingAlgorithm::Profit => {
                                OrderPriority::Profit(profit)
                            }
                            SortingAlgorithm::MevGasPrice => {
                                if gas_used > 0 {
                                    let mev_gas_price = profit / U256::from(gas_used);
                                    OrderPriority::MevGasPrice(mev_gas_price)
                                } else {
                                    OrderPriority::MevGasPrice(U256::ZERO)
                                }
                            }
                            _ => OrderPriority::GasPrice(gas_price),
                        };
                        
                        Ok((priority, Some(gas_used), Some(profit)))
                    }
                    Err(err) => {
                        debug!("Transaction simulation failed: {:?}", err);
                        Ok((OrderPriority::GasPrice(gas_price), None, None))
                    }
                }
            }
        }
    }
}