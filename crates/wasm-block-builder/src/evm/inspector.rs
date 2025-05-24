use hashbrown::HashMap;
use std::collections::hash_map::RandomState as StdRandomState;
use alloy_primitives::{Address, B256, U256};
use revm::{
    primitives::Bytes,
    interpreter::{opcode, CallInputs, CreateInputs, Interpreter, CallOutcome, CreateOutcome},
    Database, EvmContext, Inspector
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub address: Address,
    pub key: B256,
}

#[derive(Debug, Clone, Default)]
pub struct UsedStateTrace {
    pub read_slots: HashMap<SlotKey, B256, StdRandomState>,
    
    pub written_slots: HashMap<SlotKey, B256, StdRandomState>,
    
    pub read_balances: HashMap<Address, U256, StdRandomState>,
    
    pub received_amount: HashMap<Address, U256, StdRandomState>,
    pub sent_amount: HashMap<Address, U256, StdRandomState>,
    
    pub read_nonces: HashMap<Address, u64, StdRandomState>,
    pub written_nonces: HashMap<Address, u64, StdRandomState>,
    
    pub created_contracts: Vec<Address>,
    
    pub destroyed_contracts: Vec<Address>,
}

impl UsedStateTrace {
    pub fn append_trace(&mut self, other: &UsedStateTrace) {
        for (read_slot, read_value) in &other.read_slots {
            if self.read_slots.contains_key(read_slot) {
                continue;
            }
            self.read_slots.insert(read_slot.clone(), *read_value);
        }

        self.written_slots.extend(other.written_slots.clone());

        for (address, balance) in &other.read_balances {
            if self.read_balances.contains_key(address) {
                continue;
            }
            self.read_balances.insert(*address, *balance);
        }

        for (address, nonce) in &other.read_nonces {
            if self.read_nonces.contains_key(address) {
                continue;
            }
            self.read_nonces.insert(*address, *nonce);
        }

        self.written_nonces.extend(other.written_nonces.clone());

        for (address, received_amount) in &other.received_amount {
            *self.received_amount.entry(*address).or_default() += received_amount;
        }

        for (address, sent_amount) in &other.sent_amount {
            *self.sent_amount.entry(*address).or_default() += sent_amount;
        }

        self.created_contracts.extend(other.created_contracts.clone());

        for address in &other.destroyed_contracts {
            if self.destroyed_contracts.contains(address) {
                continue;
            }
            self.destroyed_contracts.push(*address);
        }
    }

    pub fn clear(&mut self) {
        self.read_slots.clear();
        self.written_slots.clear();
        self.read_balances.clear();
        self.received_amount.clear();
        self.sent_amount.clear();
        self.read_nonces.clear();
        self.written_nonces.clear();
        self.created_contracts.clear();
        self.destroyed_contracts.clear();
    }
}

#[derive(Debug, Clone, Default)]
enum NextStepAction {
    #[default]
    None,
    ReadSloadKeyResult(B256),
    ReadBalanceResult(Address),
}

#[derive(Debug)]
pub struct WasiEVMInspector<'a> {
    next_step_action: NextStepAction,
    state_trace: &'a mut UsedStateTrace,
}

impl<'a> WasiEVMInspector<'a> {
    pub fn new(state_trace: &'a mut UsedStateTrace) -> Self {
        Self {
            next_step_action: NextStepAction::None,
            state_trace,
        }
    }
    
    pub fn track_tx_nonce(&mut self, from: Address, nonce: u64) {
        self.state_trace.read_nonces.insert(from, nonce);
        self.state_trace.written_nonces.insert(from, nonce + 1);
        
    }
}

impl<'a, DB: Database> Inspector<DB> for WasiEVMInspector<'a> {
    fn step(&mut self, interpreter: &mut Interpreter, _: &mut EvmContext<DB>) {
        match std::mem::take(&mut self.next_step_action) {
            NextStepAction::ReadSloadKeyResult(slot) => {
                if let Ok(value) = interpreter.stack.peek(0) {
                    let value = B256::from(value.to_be_bytes());
                    let key = SlotKey {
                        address: interpreter.contract.target_address,
                        key: slot,
                    };
                    self.state_trace
                        .read_slots
                        .entry(key)
                        .or_insert(value);
                }
            }
            NextStepAction::ReadBalanceResult(addr) => {
                if let Ok(value) = interpreter.stack.peek(0) {
                    let bytes = value.to_be_bytes::<32>();
                    let value_u256 = U256::from_be_bytes(bytes);
                    self.state_trace
                        .read_balances
                        .entry(addr)
                        .or_insert(value_u256);
                }
            }
            NextStepAction::None => {}
        }
        match interpreter.current_opcode() {
            opcode::SLOAD => {
                if let Ok(slot) = interpreter.stack().peek(0) {
                    let slot = B256::from(slot.to_be_bytes());
                    self.next_step_action = NextStepAction::ReadSloadKeyResult(slot);
                }
            }
            opcode::SSTORE => {
                if let (Ok(slot), Ok(value)) =
                    (interpreter.stack().peek(0), interpreter.stack().peek(1))
                {
                    let written_value = B256::from(value.to_be_bytes());
                    let key = SlotKey {
                        address: interpreter.contract.target_address,
                        key: B256::from(slot.to_be_bytes()),
                    };
                    if let Some(read_value) = self.state_trace.read_slots.get(&key) {
                        if read_value == &written_value {
                            self.state_trace.written_slots.remove(&key);
                            return;
                        }
                    }
                    self.state_trace
                        .written_slots
                        .insert(key, written_value);
                }
            }
            opcode::BALANCE => {
                if let Ok(addr) = interpreter.stack().peek(0) {
                    let addr = Address::from_word(B256::from(addr.to_be_bytes()));
                    self.next_step_action = NextStepAction::ReadBalanceResult(addr);
                }
            }
            opcode::SELFBALANCE => {
                let addr = interpreter.contract().target_address;
                self.next_step_action = NextStepAction::ReadBalanceResult(addr);
            }
            _ => (),
        }
    }

    fn call(&mut self, _: &mut EvmContext<DB>, inputs: &mut CallInputs) -> Option<CallOutcome> {
        if let Some(transfer_value) = inputs.transfer_value() {
            if !transfer_value.is_zero() {
                let from = inputs.transfer_from();
                let to = inputs.transfer_to();
                
                *self
                    .state_trace
                    .sent_amount
                    .entry(from)
                    .or_default() += transfer_value;
                *self
                    .state_trace
                    .received_amount
                    .entry(to)
                    .or_default() += transfer_value;
            }
        }
        None
    }

    fn create_end(
        &mut self,
        _: &mut EvmContext<DB>,
        _: &CreateInputs,
        outcome: CreateOutcome,
    ) -> CreateOutcome {
        if let Some(addr) = outcome.address {
            self.state_trace.created_contracts.push(addr);
        }
        outcome
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        if self.state_trace.destroyed_contracts.contains(&contract) {
            return;
        }
        self.state_trace.destroyed_contracts.push(contract);
        if !value.is_zero() {
            *self
                .state_trace
                .sent_amount
                .entry(contract)
                .or_default() += value;
            *self
                .state_trace
                .received_amount
                .entry(target)
                .or_default() += value;
        }
    }
}