
use wasmer::{Instance, Module, Store, Value as WasmerValue, Memory};
use wasmer_wasix::{
    WasiEnv,
    capabilities::Capabilities,
    http::HttpClientCapabilityV1
};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    io::Write
};
use tokio;
use tracing_subscriber::{
    fmt,
    EnvFilter,
    filter::LevelFilter,
    layer::SubscriberExt,
    util::SubscriberInitExt
};

struct WasmBlockBuilderHost {
    instance: Instance,
    store: Store,
    memory: Memory,
}

#[derive(Debug)]
enum WasmError {
    ModuleLoad(String),
    FunctionNotFound(String),
    FunctionCall(String),
    BufferError(String),
    MemoryError(String),
}

impl std::error::Error for WasmError {}

impl std::fmt::Display for WasmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WasmError::ModuleLoad(msg) => write!(f, "Module load error: {}", msg),
            WasmError::FunctionNotFound(msg) => write!(f, "Function not found: {}", msg),
            WasmError::FunctionCall(msg) => write!(f, "Function call error: {}", msg),
            WasmError::BufferError(msg) => write!(f, "Buffer error: {}", msg),
            WasmError::MemoryError(msg) => write!(f, "Memory error: {}", msg),
        }
    }
}

impl From<wasmer_wasix::WasiRuntimeError> for WasmError {
    fn from(err: wasmer_wasix::WasiRuntimeError) -> Self {
        WasmError::ModuleLoad(format!("WASI error: {}", err))
    }
}

impl From<wasmer_wasix::WasiStateCreationError> for WasmError {
    fn from(err: wasmer_wasix::WasiStateCreationError) -> Self {
        WasmError::ModuleLoad(format!("WASI state creation error: {}", err))
    }
}

impl WasmBlockBuilderHost {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Result<Self, WasmError> {
        tracing_subscriber::registry()
            .with(EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy())
            .with(fmt::layer())
            .init();
        println!("Loading WASM module with Wasmer...");
        
        let wasm_bytes = fs::read(wasm_path).map_err(|e| WasmError::ModuleLoad(format!("Failed to read WASM file: {}", e)))?;
        
        let mut store = Store::default();
        println!("Using Wasmer with WAMR runtime (if available via feature flag)");
        
        let module = Module::new(&store, wasm_bytes)
            .map_err(|e| WasmError::ModuleLoad(format!("Failed to compile module: {}", e)))?;
            
        println!("Module imports:");
        for import in module.imports() {
            println!("  - {} from {}", import.name(), import.module());
        }
            
        let mut wasi_env = WasiEnv::builder("wasm_block_builder")
            .env("RUST_LOG", "debug")
            .capabilities(Capabilities {
                insecure_allow_all: true,
                http_client: HttpClientCapabilityV1::new_allow_all(),
                threading: Default::default(),
            })
            .finalize(&mut store)?;
        
        let import_object = wasi_env.import_object(&mut store, &module)
            .map_err(|e| WasmError::ModuleLoad(format!("Failed to create imports: {}", e)))?;
        
        let instance = Instance::new(&mut store, &module, &import_object)
            .map_err(|e| WasmError::ModuleLoad(format!("Failed to instantiate module: {}", e)))?;
        
        wasi_env.initialize(&mut store, instance.clone())
            .map_err(|e| WasmError::ModuleLoad(format!("Failed to initialize WASI environment: {}", e)))?;
        
        let memory = instance.exports.get_memory("memory")
            .map_err(|e| WasmError::ModuleLoad(format!("Failed to get memory: {}", e)))?
            .clone();
        
        Ok(Self {
            instance,
            store,
            memory,
        })
    }
    
    
    pub fn get_module_info(&mut self) -> Result<Value, WasmError> {
        println!("Getting WASM module info...");
        
        let output_size = 1024u32;
        
        let wbm_alloc = self.instance.exports.get_typed_function::<i32, i32>(&mut self.store, "wbm_alloc")
            .map_err(|_| WasmError::FunctionNotFound("wbm_alloc".into()))?;
        
        let output_ptr = wbm_alloc.call(&mut self.store, output_size as i32)
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let mut output_ptr = output_ptr as u32;
        
        if output_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer".into()));
        }
        
        let mut output_len = output_size;
        
        let mut output_len_ptr = output_ptr.checked_add(output_size)
            .ok_or_else(|| WasmError::MemoryError("Memory pointer arithmetic overflow".into()))?;
        
        self.write_u32_to_memory(output_len_ptr, output_len)?;
        
        let get_module_info = self.instance.exports.get_typed_function::<(i32, i32), i32>(&mut self.store, "get_module_info")
            .map_err(|_| WasmError::FunctionNotFound("get_module_info".into()))?;
            
        let result = get_module_info.call(&mut self.store, output_ptr as i32, output_len_ptr as i32)
            .map_err(|e| WasmError::FunctionCall(format!("get_module_info call failed: {}", e)))?;
        
        let wbm_free = self.instance.exports.get_typed_function::<(i32, i32), ()>(&mut self.store, "wbm_free")
            .map_err(|_| WasmError::FunctionNotFound("wbm_free".into()))?;
            
        if result == -1 {
            let _ = wbm_free.call(&mut self.store, output_ptr as i32, output_size as i32);
            return Err(WasmError::FunctionCall("Null pointer error in get_module_info".into()));
        } else if result == -2 {
            let required_size = self.read_u32_from_memory(output_len_ptr)?;
            println!("Buffer too small for module info, resizing to {} bytes...", required_size);
            output_len = required_size;
            
            let _ = wbm_free.call(&mut self.store, output_ptr as i32, output_size as i32)
                .map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
            
            let new_output_ptr = wbm_alloc.call(&mut self.store, output_len as i32)
                .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
                
            let new_output_ptr = new_output_ptr as u32;
            
            if new_output_ptr == 0 {
                return Err(WasmError::FunctionCall("wbm_alloc returned null pointer".into()));
            }
            
            let new_output_len_ptr = new_output_ptr.checked_add(output_len)
                .ok_or_else(|| WasmError::MemoryError("Memory pointer arithmetic overflow".into()))?;
            
            self.write_u32_to_memory(new_output_len_ptr, output_len)?;
            
            let result = get_module_info.call(&mut self.store, new_output_ptr as i32, new_output_len_ptr as i32)
                .map_err(|e| WasmError::FunctionCall(format!("get_module_info call failed: {}", e)))?;
            
            output_ptr = new_output_ptr;
            output_len_ptr = new_output_len_ptr;
            
            if result != 0 {
                let _ = wbm_free.call(&mut self.store, output_ptr as i32, output_len as i32);
                return Err(WasmError::FunctionCall(format!("Function returned error: {}", result)));
            }
            
            output_len = self.read_u32_from_memory(output_len_ptr)?;
        } else if result != 0 {
            let _ = wbm_free.call(&mut self.store, output_ptr as i32, output_size as i32);
            return Err(WasmError::FunctionCall(format!("Function returned error: {}", result)));
        } else {
            output_len = self.read_u32_from_memory(output_len_ptr)?;
        }
        
        let output_data = self.read_memory(output_ptr, output_len)?;
        
        let _ = wbm_free.call(&mut self.store, output_ptr as i32, output_len as i32)
            .map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        let info = serde_json::from_slice(&output_data)
            .map_err(|e| WasmError::BufferError(format!("Failed to parse JSON: {}", e)))?;
        
        Ok(info)
    }
    
    pub fn build_block(&mut self, input: &Value) -> Result<Value, WasmError> {
        let build_block = self.instance.exports.get_function("build_block")
            .map_err(|_| WasmError::FunctionNotFound("build_block".into()))?;
        
        let input_data = serde_json::to_vec(input)
            .map_err(|e| WasmError::BufferError(format!("Failed to serialize input: {}", e)))?;
        
        let wbm_alloc = self.instance.exports.get_function("wbm_alloc")
            .map_err(|_| WasmError::FunctionNotFound("wbm_alloc".into()))?;
        
        let input_size = input_data.len() as u32;
        let alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(input_size as i32)])
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let input_ptr = match alloc_results[0] {
            WasmerValue::I32(ptr) => ptr as u32,
            _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
        };
        
        if input_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer for input".into()));
        }
        
        self.write_memory(input_ptr, &input_data)?;
        
        let output_size = 64 * 1024u32;
        let alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(output_size as i32)])
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let mut output_ptr = match alloc_results[0] {
            WasmerValue::I32(ptr) => ptr as u32,
            _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer for output".into())),
        };
        
        if output_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer for output".into()));
        }
        
        let mut output_len = output_size;
        let mut output_len_ptr = output_ptr.checked_add(output_size)
            .ok_or_else(|| WasmError::MemoryError("Memory pointer arithmetic overflow".into()))?;
        
        self.write_u32_to_memory(output_len_ptr, output_len)?;
        
        let call_result = build_block.call(&mut self.store, &[
            WasmerValue::I32(input_ptr as i32),
            WasmerValue::I32(input_size as i32),
            WasmerValue::I32(output_ptr as i32),
            WasmerValue::I32(output_len_ptr as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("build_block call failed: {}", e)))?;
        
        let wbm_free = self.instance.exports.get_function("wbm_free")
            .map_err(|_| WasmError::FunctionNotFound("wbm_free".into()))?;
        
        let _ = wbm_free.call(&mut self.store, &[
            WasmerValue::I32(input_ptr as i32),
            WasmerValue::I32(input_size as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        let result = match call_result[0] {
            WasmerValue::I32(res) => res,
            _ => return Err(WasmError::FunctionCall("build_block returned invalid result".into())),
        };
        
        if result == -1 {
            return Err(WasmError::FunctionCall("Null pointer error in build_block".into()));
        } else if result == -2 {
            let required_size = self.read_u32_from_memory(output_len_ptr)?;
            println!("Buffer too small for build_block, resizing to {} bytes...", required_size);
            output_len = required_size;
            
            let _ = wbm_free.call(&mut self.store, &[
                WasmerValue::I32(output_ptr as i32),
                WasmerValue::I32(output_size as i32),
            ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
            
            let new_output_ptr = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(output_len as i32)])
                .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
                
            let new_output_ptr = match new_output_ptr[0] {
                WasmerValue::I32(ptr) => ptr as u32,
                _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
            };
            
            if new_output_ptr == 0 {
                return Err(WasmError::FunctionCall("wbm_alloc returned null pointer".into()));
            }
            
            let _ = wbm_free.call(&mut self.store, &[
                WasmerValue::I32(input_ptr as i32),
                WasmerValue::I32(input_size as i32),
            ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed for input: {}", e)))?;
            
            let input_alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(input_size as i32)])
                .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
            
            let new_input_ptr = match input_alloc_results[0] {
                WasmerValue::I32(ptr) => ptr as u32,
                _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
            };
            
            self.write_memory(new_input_ptr, &input_data)?;
            
            let new_output_len_ptr = new_output_ptr.checked_add(output_len)
                .ok_or_else(|| WasmError::MemoryError("Memory pointer arithmetic overflow".into()))?;
            
            self.write_u32_to_memory(new_output_len_ptr, output_len)?;
            
            let call_result = build_block.call(&mut self.store, &[
                WasmerValue::I32(new_input_ptr as i32),
                WasmerValue::I32(input_size as i32),
                WasmerValue::I32(new_output_ptr as i32),
                WasmerValue::I32(new_output_len_ptr as i32),
            ]).map_err(|e| WasmError::FunctionCall(format!("build_block call failed: {}", e)))?;
            
            let _ = wbm_free.call(&mut self.store, &[
                WasmerValue::I32(new_input_ptr as i32),
                WasmerValue::I32(input_size as i32),
            ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
            
            output_ptr = new_output_ptr;
            output_len_ptr = new_output_len_ptr;
            
            let new_result = match call_result[0] {
                WasmerValue::I32(res) => res,
                _ => return Err(WasmError::FunctionCall("build_block returned invalid result".into())),
            };
            
            if new_result != 0 {
                return Err(WasmError::FunctionCall(format!("Function returned error: {}", new_result)));
            }
            
            output_len = self.read_u32_from_memory(output_len_ptr)?;
        } else if result != 0 {
            return Err(WasmError::FunctionCall(format!("Function returned error: {}", result)));
        } else {
            output_len = self.read_u32_from_memory(output_len_ptr)?;
        }
        
        let output_data = self.read_memory(output_ptr, output_len)?;
        
        let output = serde_json::from_slice(&output_data)
            .map_err(|e| WasmError::BufferError(format!("Failed to parse JSON: {}", e)))?;
        
        let _ = wbm_free.call(&mut self.store, &[
            WasmerValue::I32(output_ptr as i32),
            WasmerValue::I32(output_len as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        Ok(output)
    }
    
    pub fn estimate_gas(&mut self, tx: &Value, state: &Value) -> Result<u64, WasmError> {
        let estimate_gas = self.instance.exports.get_function("estimate_gas")
            .map_err(|_| WasmError::FunctionNotFound("estimate_gas".into()))?;
        
        let tx_data = serde_json::to_vec(tx)
            .map_err(|e| WasmError::BufferError(format!("Failed to serialize tx: {}", e)))?;
        let state_data = serde_json::to_vec(state)
            .map_err(|e| WasmError::BufferError(format!("Failed to serialize state: {}", e)))?;
        
        let wbm_alloc = self.instance.exports.get_function("wbm_alloc")
            .map_err(|_| WasmError::FunctionNotFound("wbm_alloc".into()))?;
        
        let tx_size = tx_data.len() as u32;
        let tx_alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(tx_size as i32)])
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let tx_ptr = match tx_alloc_results[0] {
            WasmerValue::I32(ptr) => ptr as u32,
            _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
        };
        
        if tx_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer for tx".into()));
        }
        
        self.write_memory(tx_ptr, &tx_data)?;
        
        let state_size = state_data.len() as u32;
        let state_alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(state_size as i32)])
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let state_ptr = match state_alloc_results[0] {
            WasmerValue::I32(ptr) => ptr as u32,
            _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
        };
        
        if state_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer for state".into()));
        }
        
        self.write_memory(state_ptr, &state_data)?;
        
        let result_size = 8u32;
        let result_alloc_results = wbm_alloc.call(&mut self.store, &[WasmerValue::I32(result_size as i32)])
            .map_err(|e| WasmError::FunctionCall(format!("wbm_alloc call failed: {}", e)))?;
        
        let result_ptr = match result_alloc_results[0] {
            WasmerValue::I32(ptr) => ptr as u32,
            _ => return Err(WasmError::FunctionCall("wbm_alloc returned invalid pointer".into())),
        };
        
        if result_ptr == 0 {
            return Err(WasmError::FunctionCall("wbm_alloc returned null pointer for result".into()));
        }
        
        let call_result = estimate_gas.call(&mut self.store, &[
            WasmerValue::I32(tx_ptr as i32),
            WasmerValue::I32(tx_size as i32),
            WasmerValue::I32(state_ptr as i32),
            WasmerValue::I32(state_size as i32),
            WasmerValue::I32(result_ptr as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("estimate_gas call failed: {}", e)))?;
        
        let wbm_free = self.instance.exports.get_function("wbm_free")
            .map_err(|_| WasmError::FunctionNotFound("wbm_free".into()))?;
        
        let _ = wbm_free.call(&mut self.store, &[
            WasmerValue::I32(tx_ptr as i32),
            WasmerValue::I32(tx_size as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        let _ = wbm_free.call(&mut self.store, &[
            WasmerValue::I32(state_ptr as i32),
            WasmerValue::I32(state_size as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        let result = match call_result[0] {
            WasmerValue::I32(res) => res,
            _ => return Err(WasmError::FunctionCall("estimate_gas returned invalid result".into())),
        };
        
        if result != 0 {
            return Err(WasmError::FunctionCall(format!("Function returned error: {}", result)));
        }
        
        let gas_result = self.read_u64_from_memory(result_ptr)?;
        
        let _ = wbm_free.call(&mut self.store, &[
            WasmerValue::I32(result_ptr as i32),
            WasmerValue::I32(result_size as i32),
        ]).map_err(|e| WasmError::FunctionCall(format!("wbm_free call failed: {}", e)))?;
        
        Ok(gas_result)
    }
    
    
    fn read_u32_from_memory(&self, ptr: u32) -> Result<u32, WasmError> {
        if ptr == 0 {
            return Err(WasmError::MemoryError("Null pointer for u32 read".into()));
        }
        
        let data = self.read_memory(ptr, 4)?;
        if data.len() != 4 {
            return Err(WasmError::MemoryError(
                format!("Failed to read u32 from memory at {}: expected 4 bytes, got {}", ptr, data.len())
            ));
        }
        
        let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        Ok(value)
    }
    
    fn read_u64_from_memory(&self, ptr: u32) -> Result<u64, WasmError> {
        if ptr == 0 {
            return Err(WasmError::MemoryError("Null pointer for u64 read".into()));
        }
        
        let data = self.read_memory(ptr, 8)?;
        if data.len() != 8 {
            return Err(WasmError::MemoryError(
                format!("Failed to read u64 from memory at {}: expected 8 bytes, got {}", ptr, data.len())
            ));
        }
        
        let value = u64::from_le_bytes([
            data[0], data[1], data[2], data[3], 
            data[4], data[5], data[6], data[7]
        ]);
        Ok(value)
    }
    
    fn write_u32_to_memory(&self, ptr: u32, value: u32) -> Result<(), WasmError> {
        if ptr == 0 {
            return Err(WasmError::MemoryError("Null pointer for u32 write".into()));
        }
        
        let bytes = value.to_le_bytes();
        self.write_memory(ptr, &bytes)
    }
    
    fn read_memory(&self, ptr: u32, len: u32) -> Result<Vec<u8>, WasmError> {
        if ptr == 0 {
            return Err(WasmError::MemoryError("Null pointer".into()));
        }
        
        if len == 0 {
            return Ok(Vec::new());
        }
        
        let end_addr = ptr.checked_add(len)
            .ok_or_else(|| WasmError::MemoryError("Memory range overflow".into()))?;
            
        let memory_view = self.memory.view(&self.store);
        
        let memory_size = memory_view.size().bytes().0 as u32;
        if end_addr > memory_size {
            return Err(WasmError::MemoryError(
                format!("Memory access out of bounds: requested range {}-{}, but memory size is {}", 
                    ptr, end_addr, memory_size)
            ));
        }
        
        let mut data = vec![0u8; len as usize];
        memory_view.read(ptr as u64, &mut *data)
            .map_err(|e| WasmError::MemoryError(format!("Memory read failed: {}", e)))?;
        
        Ok(data)
    }
    
    fn write_memory(&self, ptr: u32, data: &[u8]) -> Result<(), WasmError> {
        if ptr == 0 {
            return Err(WasmError::MemoryError("Null pointer".into()));
        }
        
        if data.is_empty() {
            return Ok(());
        }
        
        let len = data.len() as u32;
        
        let end_addr = ptr.checked_add(len)
            .ok_or_else(|| WasmError::MemoryError("Memory range overflow".into()))?;
            
        let memory_view = self.memory.view(&self.store);
        
        let memory_size = memory_view.size().bytes().0 as u32;
        if end_addr > memory_size {
            return Err(WasmError::MemoryError(
                format!("Memory access out of bounds: requested range {}-{}, but memory size is {}", 
                    ptr, end_addr, memory_size)
            ));
        }
        
        memory_view.write(ptr as u64, data)
            .map_err(|e| WasmError::MemoryError(format!("Memory write failed: {}", e)))?;
        
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    
    let use_iwasm = args.iter().any(|arg| arg == "--run-iwasm");
    let build_wasm = args.iter().any(|arg| arg == "--build-wasm");
    
    if build_wasm {
        println!("Building WASM module from source...");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip1", "--release", "-p", "wasm-block-builder"])
            .status()
            .expect("Failed to execute cargo build");
            
        if !build_status.success() {
            return Err("Failed to build WASM module".into());
        }
        println!("WASM module built successfully.");
    }
    
    let wasm_path = if args.len() > 1 && !args[1].starts_with("--") {
        &args[1]
    } else {
        let wasm32_wasip1_path = "./target/wasm32-wasip1/release/wasm_block_builder.aot";
        let wasm32_wasip1_path_debug = "./target/wasm32-wasip1/debug/wasm_block_builder.aot";
        
        if Path::new(wasm32_wasip1_path).exists() {
            wasm32_wasip1_path
        } else {
            wasm32_wasip1_path_debug
        }
    };
    
    if use_iwasm {
        println!("Running with iwasm instead of Wasmer...");
        let iwasm_path = match std::process::Command::new("which")
            .arg("iwasm")
            .output()
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            },
            _ => {
                let potential_paths = [
                    "/usr/local/bin/iwasm",
                    "/usr/bin/iwasm",
                    "/home/x33f3/downloads/iwasm",
                    "./iwasm",
                ];
                
                let found_path = potential_paths.iter()
                    .find(|path| std::path::Path::new(path).exists())
                    .cloned();
                
                match found_path {
                    Some(path) => path.to_string(),
                    None => {
                        eprintln!("iwasm not found. Please install WAMR (WebAssembly Micro Runtime) or specify the path to iwasm.");
                        eprintln!("For installation instructions, see: https://github.com/bytecodealliance/wasm-micro-runtime");
                        return Err("iwasm not found".into());
                    }
                }
            }
        };
        
        println!("Using iwasm at: {}", iwasm_path);
        
        let output = std::process::Command::new(&iwasm_path)
            .arg(wasm_path)
            .output()
            .expect("Failed to execute iwasm");
            
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            println!("iwasm output:\n{}", stdout);
            return Ok(());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("iwasm error:\n{}", stderr);
            return Err("iwasm execution failed".into());
        }
    }
    
    let sample_tx = json!({
        "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
        "from": "0x1111111111111111111111111111111111111111",
        "to": "0x2222222222222222222222222222222222222222",
        "value": "0x1bc16d674ec80000",
        "nonce": 5,
        "gas_limit": 21000,
        "gas_price": "0x3b9aca00",
        "input": "0x",
        "tx_type": "Legacy",
        "access_list": [],
        "blob_hashes": [],
        "max_priority_fee_per_gas": null,
        "max_fee_per_blob_gas": null,
        "versioned_hashes": []
    });
    
    let keccak_empty = "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470";
    let sample_state = json!({
        "accounts": [
            {
                "address": "0x1111111111111111111111111111111111111111",
                "balance": "0x3635c9adc5dea00000",
                "nonce": 5,
                "code_hash": keccak_empty
            },
            {
                "address": "0x2222222222222222222222222222222222222222",
                "balance": "0x56bc75e2d63100000",
                "nonce": 0,
                "code_hash": keccak_empty
            }
        ],
        "storage": [],
        "code": []
    });
    
    let sample_input = json!({
        "block_params": {
            "number": 1000000,
            "timestamp": 1681234567,
            "gas_limit": 30000000,
            "base_fee_per_gas": "0x3b9aca00",
            "coinbase": "0x3333333333333333333333333333333333333333",
            "parent_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "parent_state_root": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "withdrawals_root": null,
            "blob_gas_used": null,
            "excess_blob_gas": null
        },
        "accounts": [
            {
                "address": "0x1111111111111111111111111111111111111111",
                "balance": "0x3635c9adc5dea00000",
                "nonce": 5,
                "code_hash": keccak_empty
            },
            {
                "address": "0x2222222222222222222222222222222222222222",
                "balance": "0x56bc75e2d63100000",
                "nonce": 0,
                "code_hash": keccak_empty
            },
            {
                "address": "0x3333333333333333333333333333333333333333",
                "balance": "0x0",
                "nonce": 0,
                "code_hash": keccak_empty
            }
        ],
        "storage": [],
        "code": [],
        "transactions": [
            {
                "hash": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef",
                "from": "0x1111111111111111111111111111111111111111",
                "to": "0x2222222222222222222222222222222222222222",
                "value": "0x1bc16d674ec80000",
                "gas_limit": 21000,
                "gas_price": "0x3b9aca00",
                "nonce": 5,
                "input": "0x",
                "tx_type": "Legacy",
                "access_list": [],
                "blob_hashes": [],
                "max_priority_fee_per_gas": null,
                "max_fee_per_blob_gas": null,
                "versioned_hashes": [],
                "encoded_signed_tx": "0xf86b058503b9aca00825208942222222222222222222222222222222222222228801bc16d674ec800080819aa01234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdefa01234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
            }
        ],
        "bundles": [],
        "config": {
            "discard_txs": true,
            "sorting": "MevGasPrice",
            "failed_tx_retries": 1,
            "drop_failed_txs": true,
            "coinbase_payment": false,
            "build_timeout_ms": 1000
        }
    });
    
    println!("Starting WASM Block Builder demo...");
    
    if !Path::new(wasm_path).exists() {
        println!("WASM file not found at {}. Have you compiled the project with wasm32-wasip1 target?", wasm_path);
        println!("  cargo build --target wasm32-wasip1 --release -p wasm-block-builder");
        println!("\nOr use this demo with one of the following options:");
        println!("  cargo run --example demo --features=\"wamr\" -- [WASM_PATH]  # Run with specified WASM file");
        println!("  cargo run --example demo --features=\"wamr\" -- --build-wasm  # Build the WASM module first");
        println!("  cargo run --example demo --features=\"wamr\" -- --run-iwasm   # Run using iwasm directly");
        return Ok(());
    }
    
    println!("Loading WASM module from: {}", wasm_path);
    println!("Using WAMR backend from Wasmer for execution (falls back to default if needed)");
    
    let mut host = WasmBlockBuilderHost::new(wasm_path)?;
    
    println!("Getting module info...");
    let module_info = host.get_module_info()?;
    println!("Module info: {}", serde_json::to_string_pretty(&module_info)?);
    
    println!("\nEstimating gas for transaction...");
    let gas_estimate = host.estimate_gas(&sample_tx, &sample_state)?;
    println!("Gas estimate: {}", gas_estimate);
    
    println!("\nBuilding block...");
    let block_result = host.build_block(&sample_input)?;
    
    println!("Block built successfully!");
    println!("Block number: {}", block_result["header"]["number"]);
    println!("Transactions included: {}", block_result["metrics"]["tx_count"]);
    println!("Gas used: {}", block_result["metrics"]["gas_used"]);
    println!("Block value: {}", block_result["metrics"]["block_value"]);
    println!("Build time (μs): {}", block_result["metrics"]["build_time_us"]);
    
    let output_path = "wasm_block_builder_output.json";
    fs::write(output_path, serde_json::to_string_pretty(&block_result)?)?;
    println!("\nFull block output saved to: {}", output_path);
    
    Ok(())
}
