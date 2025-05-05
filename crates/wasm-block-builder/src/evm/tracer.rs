use revm::{
    primitives::Bytes,
    interpreter::CreateInputs,
    interpreter::{CallInputs, Gas, InstructionResult, Interpreter},
    Database, EvmContext, Inspector,
};

#[derive(Debug, Default)]
pub struct WasiExecutionTracer {
    pub gas_used: u64,
    
    pub blob_gas_used: u64,
    
    pub op_count: usize,

}

impl<DB: Database> Inspector<DB> for WasiExecutionTracer {
    fn step(&mut self, _interp: &mut Interpreter, _context: &mut EvmContext<DB>) {
        self.op_count += 1;
        
    }
    
    fn call_end(&mut self, _context: &mut EvmContext<DB>, inputs: &CallInputs, outcome: revm::interpreter::CallOutcome) -> revm::interpreter::CallOutcome {
        let gas_used_in_call = inputs.gas_limit.saturating_sub(outcome.gas().remaining());
        self.gas_used += gas_used_in_call;
        outcome
    }
    
    fn create_end(&mut self, _context: &mut EvmContext<DB>, inputs: &CreateInputs, outcome: revm::interpreter::CreateOutcome) -> revm::interpreter::CreateOutcome {
        let gas_used_in_create = inputs.gas_limit.saturating_sub(outcome.gas().remaining());
        self.gas_used += gas_used_in_create;
        outcome
    }
}
