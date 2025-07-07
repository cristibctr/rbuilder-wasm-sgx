use crate::{
    evm,
    interfaces::input::{BlockParams, SerializedBundle, SerializedTransaction},
    state::WasiStateProvider,
};
use block_builder_types::{SortingAlgorithm, OrderForOrdering};
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

#[derive(Debug, Clone)]
pub struct OrderedTransactionMeta {
    pub id: String,
    pub order_type: String,
    pub priority: OrderPriority,
    pub coinbase_profit: U256,
    pub gas_used: u64,
    pub mev_gas_price: U256,
}

#[derive(Debug, Clone)]
pub struct OrderingResult {
    pub ordered_ids: Vec<String>,
    pub ordered_transactions: Vec<OrderedTransactionMeta>,
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
    base_fee: U256,
    block_params: Option<BlockParams>,
}

impl WasiOrderSorter {
    pub fn new(algorithm: SortingAlgorithm) -> Self {
        Self { 
            algorithm,
            base_fee: U256::from(1_000_000_000),
            block_params: None,
        }
    }
    
    pub fn with_base_fee(mut self, base_fee: U256) -> Self {
        self.base_fee = base_fee;
        self
    }
    
    pub fn with_block_params(mut self, block_params: BlockParams) -> Self {
        self.base_fee = block_params.base_fee_per_gas;
        self.block_params = Some(block_params);
        self
    }
    
    pub fn sort_orders(
        &self,
        orders: Vec<OrderForOrdering>,
    ) -> Result<OrderingResult, BlockBuilderError> {
        info!("SGX performing pure algorithmic ordering of {} orders using {:?}", orders.len(), self.algorithm);
        
        let mut ordered_txs: Vec<OrderedTransactionMeta> = orders.clone()
            .into_iter()
            .map(|order| {
                let priority = self.calculate_priority_from_precalc(&order);
                OrderedTransactionMeta {
                    id: order.id,
                    order_type: order.order_type,
                    priority,
                    coinbase_profit: order.coinbase_profit,
                    gas_used: order.gas_used,
                    mev_gas_price: if order.gas_used > 0 {
                        order.coinbase_profit / U256::from(order.gas_used)
                    } else {
                        U256::ZERO
                    },
                }
            })
            .collect();

        ordered_txs = self.sort_with_nonce_constraints(ordered_txs, &orders)?;
        
        info!("SGX completed ordering: algorithm={:?}, total_orders={}", self.algorithm, ordered_txs.len());
        
        for (i, tx) in ordered_txs.iter().take(5).enumerate() {
            debug!("SGX ordered #{}: id={}, priority={:?}, profit={}, gas={}", 
                i + 1, tx.id, tx.priority, tx.coinbase_profit, tx.gas_used);
        }
        
        let ordered_ids = ordered_txs.iter().map(|tx| tx.id.clone()).collect();
        
        Ok(OrderingResult {
            ordered_ids,
            ordered_transactions: ordered_txs,
        })
    }
    
    fn calculate_priority_from_precalc(&self, order: &OrderForOrdering) -> OrderPriority {
        match self.algorithm {
            SortingAlgorithm::GasPrice => {
                OrderPriority::GasPrice(order.gas_price)
            }
            SortingAlgorithm::Profit => {
                OrderPriority::Profit(order.coinbase_profit)
            }
            SortingAlgorithm::MevGasPrice => {
                let mev_gas_price = if order.gas_used > 0 {
                    order.coinbase_profit / U256::from(order.gas_used)
                } else {
                    U256::ZERO
                };
                OrderPriority::MevGasPrice(mev_gas_price)
            }
        }
    }
    
    fn sort_with_nonce_constraints(
        &self,
        mut transactions: Vec<OrderedTransactionMeta>,
        original_orders: &[OrderForOrdering],
    ) -> Result<Vec<OrderedTransactionMeta>, BlockBuilderError> {
        use std::collections::HashMap;

        let mut nonce_info: HashMap<String, (Address, u64)> = HashMap::new();
        for order in original_orders {
            if let (Some(from_address), Some(nonce)) = (order.from_address, order.nonce) {
                nonce_info.insert(order.id.clone(), (from_address, nonce));
            }
        }

        let mut account_txs: HashMap<Address, Vec<OrderedTransactionMeta>> = HashMap::new();
        let mut other_txs = Vec::new();
        
        for tx in transactions {
            if let Some((address, _)) = nonce_info.get(&tx.id) {
                account_txs.entry(*address).or_insert_with(Vec::new).push(tx);
            } else {
                other_txs.push(tx);
            }
        }

        for (address, txs) in account_txs.iter_mut() {
            txs.sort_by(|a, b| {
                let nonce_a = nonce_info.get(&a.id).map(|(_, n)| *n).unwrap_or(0);
                let nonce_b = nonce_info.get(&b.id).map(|(_, n)| *n).unwrap_or(0);
                nonce_a.cmp(&nonce_b)
            });
        }

        let mut result = Vec::new();
        let mut account_indices: HashMap<Address, usize> = HashMap::new();

        other_txs.sort_by(|a, b| b.priority.cmp(&a.priority));

        let mut all_account_txs: Vec<(Address, &OrderedTransactionMeta)> = Vec::new();
        for (address, txs) in &account_txs {
            for tx in txs {
                all_account_txs.push((*address, tx));
            }
        }

        all_account_txs.sort_by(|a, b| b.1.priority.cmp(&a.1.priority));

        let mut processed_accounts = std::collections::HashSet::new();
        
        for (address, _) in all_account_txs {
            if processed_accounts.contains(&address) {
                continue;
            }
            
            if let Some(txs) = account_txs.get(&address) {
                result.extend(txs.iter().cloned());
                processed_accounts.insert(address);
            }
        }

        result.extend(other_txs);
        
        info!("SGX nonce-aware ordering: processed {} transactions with nonce constraints", result.len());
        
        Ok(result)
    }
    
    pub fn sort_transactions(
        &self,
        transactions: Vec<SerializedTransaction>,
        bundles: Vec<SerializedBundle>,
        state: &mut WasiStateProvider,
    ) -> Result<Vec<OrderedTransaction>, BlockBuilderError> {
        let mut ordered_txs = Vec::new();
        
        let default_block_params = BlockParams {
            number: 1,
            timestamp: 1,
            gas_limit: 30_000_000,
            base_fee_per_gas: self.base_fee,
            coinbase: Address::ZERO,
            parent_hash: B256::ZERO,
            parent_state_root: B256::ZERO,
            withdrawals_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
            prev_randao: B256::ZERO,
        };
        let block_params = self.block_params.as_ref().unwrap_or(&default_block_params);
        
        for tx in transactions {
            let (priority, gas_used, profit) = self.calculate_priority(&tx, state, block_params)?;
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
                match self.calculate_priority(tx, state, block_params) {
                    Ok((priority, gas_used, profit)) => {
                        if let Some(profit_val) = profit {
                            total_profit += profit_val;
                        }
                        ordered_txs.push(OrderedTransaction {
                            transaction: tx.clone(),
                            priority,
                            in_bundle: is_bundle,
                            bundle_hash: Some(bundle.hash),
                            simulated_gas_used: gas_used,
                            simulated_profit: profit,
                        });
                    }
                    Err(_) => {
                        any_failed = true;
                        break;
                    }
                }
            }
            
            if any_failed {
                ordered_txs.retain(|tx| tx.bundle_hash != Some(bundle.hash));
            }
        }
        
        ordered_txs.sort_by(|a, b| b.priority.cmp(&a.priority));
        
        Ok(ordered_txs)
    }
    
    fn calculate_priority(
        &self, 
        tx: &SerializedTransaction,
        state: &mut WasiStateProvider,
        block_params: &BlockParams,
    ) -> Result<(OrderPriority, Option<u64>, Option<U256>), BlockBuilderError> {
        let gas_price = match tx.max_priority_fee_per_gas {
            Some(priority_fee) => {
                block_params.base_fee_per_gas + priority_fee
            }
            None => {
                tx.gas_price.unwrap_or_default()
            }
        };
        
        match self.algorithm {
            SortingAlgorithm::GasPrice => {
                Ok((OrderPriority::GasPrice(gas_price), None, None))
            }
            SortingAlgorithm::Profit | SortingAlgorithm::MevGasPrice => {
                info!("Simulating transaction for profit calculation: {:?}", tx.hash);
                
                let mut state_copy = state.clone();
                
                match evm::execute_transaction(tx, &mut state_copy, block_params) {
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