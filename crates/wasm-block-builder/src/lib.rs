
mod builder;
mod crypto;
mod evm;
mod interfaces;
mod state;

use interfaces::{deserialize_block_input, deserialize_state_input, deserialize_state_changes, serialize_output};
use state::provider::WasiStateProvider;
use thiserror::Error;
use std::sync::{Arc, Mutex, Once};
use lazy_static::lazy_static;

lazy_static! {
    static ref LAST_SIMULATOR: Mutex<Option<Arc<builder::WasiSimulator>>> = Mutex::new(None);
    static ref SIMULATOR_INIT: Once = Once::new();
}

// Import the interfaces WasiError for conversion
use interfaces::WasiError as InterfacesWasiError;
use crate::interfaces::serialize_output_without_signature;

#[derive(Error, Debug)]
pub enum WasiError {
    #[error("Input Deserialization Error: {0}")]
    InputDeserialization(String),
    #[error("Output Serialization Error: {0}")]
    OutputSerialization(String),
    #[error("Block Builder Error: {0}")]
    Builder(#[from] builder::BlockBuilderError),
    #[error("EVM Error: {0}")]
    Evm(#[from] evm::EVMError),
    #[error("State Root Error: {0}")]
    StateRoot(String),
    #[error("State Provider Error: {0}")]
    State(#[from] state::provider::StateError),
    #[error("Crypto Error: {0}")]
    Crypto(#[from] crypto::CryptoError),
    #[error("Invalid Input Pointer")]
    NullInputPtr,
    #[error("Invalid Output Pointer")]
    NullOutputPtr,
    #[error("Output Buffer Too Small: required {required}, got {provided}")]
    OutputBufferTooSmall { required: usize, provided: usize },
}

type WasiResult<T> = Result<T, WasiError>;

// Implement conversion from interfaces::WasiError to lib::WasiError
impl From<InterfacesWasiError> for WasiError {
    fn from(err: InterfacesWasiError) -> Self {
        match err {
            InterfacesWasiError::InputDeserialization(msg) => WasiError::InputDeserialization(msg),
            InterfacesWasiError::OutputSerialization(msg) => WasiError::OutputSerialization(msg),
            InterfacesWasiError::Builder(msg) => WasiError::InputDeserialization(format!("Builder error: {}", msg)),
            InterfacesWasiError::Evm(msg) => WasiError::InputDeserialization(format!("EVM error: {}", msg)),
            InterfacesWasiError::StateRoot(msg) => WasiError::StateRoot(msg),
            InterfacesWasiError::State(msg) => WasiError::InputDeserialization(format!("State error: {}", msg)),
            InterfacesWasiError::NullInputPtr => WasiError::NullInputPtr,
            InterfacesWasiError::NullOutputPtr => WasiError::NullOutputPtr,
            InterfacesWasiError::OutputBufferTooSmall { required, provided } => 
                WasiError::OutputBufferTooSmall { required, provided },
            InterfacesWasiError::Internal(msg) => WasiError::InputDeserialization(format!("Internal error: {}", msg)),
        }
    }
}

#[no_mangle]
pub extern "C" fn build_block(
    input_ptr: *const u8, 
    input_len: usize,
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> i32 {
    match process_build_block_safe(input_ptr, input_len, output_ptr, output_len_ptr) {
        Ok(_) => 0,
        Err(e) => {
            log::error!("build_block failed: {}", e);
            match e {
                WasiError::NullInputPtr => -1,
                WasiError::NullOutputPtr => -1,
                WasiError::OutputBufferTooSmall { .. } => -2,
                WasiError::InputDeserialization(_) => -3,
                WasiError::Builder(_) => -4,
                WasiError::Evm(_) => -5,
                WasiError::State(_) => -6,
                WasiError::StateRoot(_) => -7,
                WasiError::OutputSerialization(_) => -8,
                WasiError::Crypto(_) => -9,
            }
        }
    }
}

fn process_build_block_safe(
    input_ptr: *const u8, 
    input_len: usize,
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> WasiResult<()> {
    if input_ptr.is_null() { return Err(WasiError::NullInputPtr); }
    if output_ptr.is_null() || output_len_ptr.is_null() { return Err(WasiError::NullOutputPtr); }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    
    let output_vec = process_build_block_internal(input_slice)?;
    
    unsafe {
        let provided_len = *output_len_ptr;
        let required_len = output_vec.len();
        
        if required_len > provided_len {
            *output_len_ptr = required_len;
            return Err(WasiError::OutputBufferTooSmall { required: required_len, provided: provided_len });
        }
        
        std::ptr::copy_nonoverlapping(output_vec.as_ptr(), output_ptr, required_len);
        *output_len_ptr = required_len;
    }
    
    Ok(())
}


fn process_build_block_internal(input: &[u8]) -> WasiResult<Vec<u8>> {
    let input_data = deserialize_block_input(input)
        .map_err(|e| WasiError::InputDeserialization(format!("Failed to deserialize BlockBuilderInput: {}", e)))?;
    
    log::info!("Starting block build for block {}", input_data.block_params.number);
    
    let state_provider = WasiStateProvider::new(
        input_data.accounts,
        input_data.storage,
        input_data.code,
    );
    
    let mut builder = builder::WasiBlockBuilder::new(
        state_provider,
        input_data.block_params.clone(),
        input_data.config.clone(),
    );
    
    let mut output_data = builder
        .build_block(input_data.transactions, input_data.bundles)?;

    if output_data.chunk_info.is_some() && output_data.build_id.is_some() {
        let simulator_ref = Arc::new((*builder.simulator()).clone());

        if let Ok(mut guard) = LAST_SIMULATOR.lock() {
            *guard = Some(simulator_ref.clone());
            log::info!("Stored simulator instance for build ID: {}", output_data.build_id.as_ref().unwrap());
        } else {
            log::warn!("Failed to store simulator instance for chunk retrieval");
        }
    }

    log::info!("Signing block {}", output_data.header.number);

    let signer = crypto::BlockSigner::new()?;

    let data_to_sign = serialize_output_without_signature(&output_data)?;

    let signature = signer.sign(&data_to_sign)?;

    output_data.signature = Some(signature);
        
    log::info!("Finished block build for block {} with signature", output_data.header.number);

    serialize_output(&output_data)
        .map_err(|e| WasiError::OutputSerialization(format!("Failed to serialize BlockBuilderOutput: {}", e)))
}

#[no_mangle]
pub extern "C" fn estimate_gas(
    tx_data_ptr: *const u8, 
    tx_data_len: usize,
    state_data_ptr: *const u8,
    state_data_len: usize,
    result_ptr: *mut u64
) -> i32 {
     match process_estimate_gas_safe(tx_data_ptr, tx_data_len, state_data_ptr, state_data_len, result_ptr) {
        Ok(_) => 0,
        Err(e) => {
            log::error!("estimate_gas failed: {}", e);
            match e {
                WasiError::NullInputPtr => -1,
                WasiError::NullOutputPtr => -1,
                WasiError::InputDeserialization(_) => -3,
                WasiError::Evm(evm_err) => match evm_err {
                     evm::EVMError::GasEstimation(_) => -5,
                     _ => -6,
                },
                WasiError::State(_) => -7,
                _ => -99,
            }
        }
    }
}

fn process_estimate_gas_safe(
    tx_data_ptr: *const u8, 
    tx_data_len: usize,
    state_data_ptr: *const u8,
    state_data_len: usize,
    result_ptr: *mut u64
) -> WasiResult<()> {
    if tx_data_ptr.is_null() || state_data_ptr.is_null() { return Err(WasiError::NullInputPtr); }
    if result_ptr.is_null() { return Err(WasiError::NullOutputPtr); }

    let tx_data = unsafe { std::slice::from_raw_parts(tx_data_ptr, tx_data_len) };
    let state_data = unsafe { std::slice::from_raw_parts(state_data_ptr, state_data_len) };
    
    let gas_estimate = process_estimate_gas_internal(tx_data, state_data)?;
    
    unsafe { *result_ptr = gas_estimate; }
    
    Ok(())
}

#[no_mangle]
pub extern "C" fn get_module_info(
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> i32 {
    struct ModuleInfo {
        version: String,
        api_version: u32,
        features: Vec<String>,
        supported_tx_types: Vec<String>,
        security_features: Vec<String>,
        public_key: Option<String>,
    }
    
    impl serde::Serialize for ModuleInfo {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(Some(6))?;
            map.serialize_entry("version", &self.version)?;
            map.serialize_entry("api_version", &self.api_version)?;
            map.serialize_entry("features", &self.features)?;
            map.serialize_entry("supported_tx_types", &self.supported_tx_types)?;
            map.serialize_entry("security_features", &self.security_features)?;
            map.serialize_entry("public_key", &self.public_key)?;
            map.end()
        }
    }

    let public_key = match crypto::export_public_key_hex() {
        Ok(key) => Some(key),
        Err(e) => {
            log::warn!("Failed to get public key: {}", e);
            None
        }
    };
    
    let info = ModuleInfo {
        version: "0.1.0".to_string(),
        api_version: 1,
        features: vec![
            "transaction_simulation".to_string(),
            "gas_estimation".to_string(),
            "state_root".to_string(),
            "eip1559_support".to_string(),
            "eip4844_support".to_string(),
        ],
        supported_tx_types: vec![
            "legacy".to_string(),
            "access_list".to_string(),
            "eip1559".to_string(),
            "blob".to_string(),
        ],
        security_features: vec![
            "ecdsa_output_signing".to_string(),
            "secure_key_management".to_string(),
        ],
        public_key,
    };
    
    match serde_json::to_vec(&info) {
        Ok(json_data) => {
            println!("{:?}", json_data);
            if output_ptr.is_null() || output_len_ptr.is_null() {
                return -1;
            }
            
            unsafe {
                let provided_len = *output_len_ptr;
                let required_len = json_data.len();
                
                if required_len > provided_len {
                    *output_len_ptr = required_len;
                    return -2;
                }
                
                std::ptr::copy_nonoverlapping(json_data.as_ptr(), output_ptr, required_len);
                *output_len_ptr = required_len;
            }
            
            0
        },
        Err(_) => -3,
    }
}

#[no_mangle]
pub extern "C" fn get_public_key(
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> i32 {
    if output_ptr.is_null() || output_len_ptr.is_null() {
        return -1;
    }

    match crypto::export_public_key_hex() {
        Ok(key) => {
            let key_bytes = key.as_bytes();
            unsafe {
                let provided_len = *output_len_ptr;
                let required_len = key_bytes.len();
                
                if required_len > provided_len {
                    *output_len_ptr = required_len;
                    return -2;
                }
                
                std::ptr::copy_nonoverlapping(key_bytes.as_ptr(), output_ptr, required_len);
                *output_len_ptr = required_len;
            }
            0
        },
        Err(_) => -3,
    }
}

#[no_mangle]
pub extern "C" fn get_state_diff_chunk(
    chunk_id: u32,
    build_id_ptr: *const u8,
    build_id_len: usize,
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> i32 {
    match process_get_state_diff_chunk_safe(chunk_id, build_id_ptr, build_id_len, output_ptr, output_len_ptr) {
        Ok(_) => 0,
        Err(e) => {
            log::error!("get_state_diff_chunk failed: {}", e);
            match e {
                WasiError::NullInputPtr => -1,
                WasiError::NullOutputPtr => -1,
                WasiError::OutputBufferTooSmall { .. } => -2,
                WasiError::InputDeserialization(_) => -3,
                WasiError::OutputSerialization(_) => -4,
                _ => -99,
            }
        }
    }
}

fn process_get_state_diff_chunk_safe(
    chunk_id: u32,
    build_id_ptr: *const u8,
    build_id_len: usize,
    output_ptr: *mut u8,
    output_len_ptr: *mut usize
) -> WasiResult<()> {
    if build_id_ptr.is_null() { return Err(WasiError::NullInputPtr); }
    if output_ptr.is_null() || output_len_ptr.is_null() { return Err(WasiError::NullOutputPtr); }
    
    let build_id_slice = unsafe { std::slice::from_raw_parts(build_id_ptr, build_id_len) };
    let build_id = std::str::from_utf8(build_id_slice)
        .map_err(|e| WasiError::InputDeserialization(format!("Invalid build ID string: {}", e)))?;
    
    let simulator = match LAST_SIMULATOR.lock() {
        Ok(guard) => guard,
        Err(e) => return Err(WasiError::InputDeserialization(format!("Failed to acquire simulator lock: {}", e))),
    };
    
    if simulator.is_none() {
        return Err(WasiError::InputDeserialization("No previous build found".to_string()));
    }
    
    let chunk = match simulator.as_ref().unwrap().get_state_diff_chunk(build_id, chunk_id) {
        Some(chunk) => chunk,
        None => return Err(WasiError::InputDeserialization(format!("Chunk ID {} not found for build ID {}", chunk_id, build_id))),
    };
    
    let chunk_json = serde_json::to_vec(&chunk)
        .map_err(|e| WasiError::OutputSerialization(format!("Failed to serialize chunk: {}", e)))?;
    
    unsafe {
        let provided_len = *output_len_ptr;
        let required_len = chunk_json.len();
        
        if required_len > provided_len {
            *output_len_ptr = required_len;
            return Err(WasiError::OutputBufferTooSmall { required: required_len, provided: provided_len });
        }
        
        std::ptr::copy_nonoverlapping(chunk_json.as_ptr(), output_ptr, required_len);
        *output_len_ptr = required_len;
    }
    
    Ok(())
}


fn process_estimate_gas_internal(tx_data: &[u8], state_data: &[u8]) -> WasiResult<u64> {
    let tx = serde_json::from_slice(tx_data)
        .map_err(|e| WasiError::InputDeserialization(format!("Failed to deserialize transaction: {}", e)))?;
    
    log::debug!("Transaction parsed successfully: {:?}", tx);
    
    let state_input = deserialize_state_input(state_data)
        .map_err(|e| WasiError::InputDeserialization(format!("Failed to deserialize state input: {}", e)))?;
    
    log::debug!("State input parsed successfully with {} accounts", state_input.accounts.len());
    
    let state_provider = WasiStateProvider::new(
        state_input.accounts,
        state_input.storage,
        state_input.code,
    );

    let gas = evm::estimate_gas(&tx, &state_provider)?;
    Ok(gas)
}

#[no_mangle]
pub extern "C" fn calculate_state_root(
    changes_ptr: *const u8, 
    changes_len: usize,
    state_ptr: *const u8,
    state_len: usize,
    result_ptr: *mut u8,
    result_len_ptr: *mut usize
) -> i32 {
     match process_calculate_state_root_safe(changes_ptr, changes_len, state_ptr, state_len, result_ptr, result_len_ptr) {
        Ok(_) => 0,
        Err(e) => {
            log::error!("calculate_state_root failed: {}", e);
            match e {
                WasiError::NullInputPtr => -1,
                WasiError::NullOutputPtr => -1,
                WasiError::OutputBufferTooSmall { .. } => -2,
                WasiError::InputDeserialization(_) => -3,
                WasiError::StateRoot(_) => -4,
                _ => -99,
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn set_log_level(level: i32) -> i32 {
    match level {
        0..=5 => 0,
        _ => -1,
    }
}

#[no_mangle]
pub extern "C" fn wbm_alloc(size: usize) -> *mut u8 {
    
    if size > 100 * 1024 * 1024 {
        log::warn!("Allocation request of {} bytes exceeds 100MB limit, refusing", size);
        return std::ptr::null_mut();
    }
    
    if size == 0 {
        return std::ptr::null_mut();
    }
    
    let layout = match std::alloc::Layout::from_size_align(size, 8) {
        Ok(layout) => layout,
        Err(e) => {
            log::error!("Invalid layout parameters for allocation: {}", e);
            return std::ptr::null_mut();
        }
    };
    
    let ptr = unsafe { std::alloc::alloc(layout) };
    
    if ptr.is_null() {
        log::error!("Memory allocation of {} bytes failed", size);
        std::ptr::null_mut()
    } else {
        log::debug!("Successfully allocated {} bytes at {:p}", size, ptr);
        ptr
    }
}

#[no_mangle]
pub extern "C" fn wbm_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        log::debug!("Attempted to free null pointer, ignoring");
        return;
    }
    
    if size == 0 {
        log::debug!("Attempted to free zero-sized allocation, ignoring");
        return;
    }
    
    if size > 100 * 1024 * 1024 {
        log::warn!("Attempted to free suspiciously large allocation of {} bytes, refusing", size);
        return;
    }
    
    let layout = match std::alloc::Layout::from_size_align(size, 8) {
        Ok(layout) => layout,
        Err(e) => {
            log::error!("Invalid layout parameters for deallocation: {}", e);
            return;
        }
    };
    
    log::debug!("Freeing {} bytes at {:p}", size, ptr);
    unsafe {
        std::alloc::dealloc(ptr, layout);
    }
}

#[no_mangle]
pub extern "C" fn wbm_get_byte(offset: u32) -> u8 {
    unsafe {
        let ptr = offset as *const u8;
        if ptr.is_null() {
            return 0;
        }
        *ptr
    }
}

#[no_mangle]
pub extern "C" fn wbm_set_byte(value: u8, offset: u32) -> i32 {
    unsafe {
        let ptr = offset as *mut u8;
        if ptr.is_null() {
            return -1;
        }
        *ptr = value;
        0
    }
}

#[no_mangle]
pub extern "C" fn wbm_memory_copy(dest: u32, src: u32, len: u32) -> i32 {
    unsafe {
        let dest_ptr = dest as *mut u8;
        let src_ptr = src as *const u8;
        
        if dest_ptr.is_null() || src_ptr.is_null() || len == 0 {
            return -1;
        }
        
        std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize);
        0
    }
}

#[no_mangle]
pub extern "C" fn wbm_read_buffer(src: u32, dest: u32, len: u32) -> i32 {
    unsafe {
        let src_ptr = src as *const u8;
        let dest_ptr = dest as *mut u8;
        
        if src_ptr.is_null() || dest_ptr.is_null() || len == 0 {
            return -1;
        }
        
        std::ptr::copy_nonoverlapping(src_ptr, dest_ptr, len as usize);
        0
    }
}

fn process_calculate_state_root_safe(
    changes_ptr: *const u8, 
    changes_len: usize,
    state_ptr: *const u8,
    state_len: usize,
    result_ptr: *mut u8,
    result_len_ptr: *mut usize
) -> WasiResult<()> {
    if changes_ptr.is_null() || state_ptr.is_null() { return Err(WasiError::NullInputPtr); }
    if result_ptr.is_null() || result_len_ptr.is_null() { return Err(WasiError::NullOutputPtr); }

    let changes_slice = unsafe { std::slice::from_raw_parts(changes_ptr, changes_len) };
    let state_slice = unsafe { std::slice::from_raw_parts(state_ptr, state_len) };
    
    let root_bytes = process_calculate_state_root_internal(changes_slice, state_slice)?;
    
    unsafe {
        let provided_len = *result_len_ptr;
        let required_len = root_bytes.len();

        if required_len != 32 {
             return Err(WasiError::StateRoot("Internal error: state root calculation did not return 32 bytes".into()));
        }

        if required_len > provided_len {
            *result_len_ptr = required_len;
            return Err(WasiError::OutputBufferTooSmall { required: required_len, provided: provided_len });
        }
        
        std::ptr::copy_nonoverlapping(root_bytes.as_ptr(), result_ptr, required_len);
        *result_len_ptr = required_len;
    }
    
    Ok(())
}


fn process_calculate_state_root_internal(changes: &[u8], state: &[u8]) -> WasiResult<Vec<u8>> {
    let state_changes = deserialize_state_changes(changes)
        .map_err(|e| WasiError::InputDeserialization(format!("Failed to deserialize state changes: {}", e)))?;
    
    let base_state = deserialize_state_input(state)
        .map_err(|e| WasiError::InputDeserialization(format!("Failed to deserialize base state: {}", e)))?;
    
    let state_provider = WasiStateProvider::new(
        base_state.accounts,
        base_state.storage,
        base_state.code,
    );

    let root = state::root::calculate_state_root(&state_changes, &state_provider)
        .map_err(|e| WasiError::StateRoot(format!("State root calculation failed: {}", e)))?;
    
    Ok(root.0.to_vec())
}
