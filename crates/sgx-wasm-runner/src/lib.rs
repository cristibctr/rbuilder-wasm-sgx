use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::io;
use std::ptr;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use log::{debug, error, warn};

lazy_static::lazy_static! {
    static ref METRICS: Mutex<HashMap<String, AtomicU64>> = Mutex::new(HashMap::new());
}

fn metrics_inc_counter(name: &str, value: u64) {
    let mut metrics = METRICS.lock().unwrap_or_else(|e| {
        error!("[SGX Metrics] Failed to lock metrics: {}", e);
        return e.into_inner();
    });
    
    metrics
        .entry(name.to_string())
        .or_insert_with(|| AtomicU64::new(0))
        .fetch_add(value, Ordering::Relaxed);
}

#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(dead_code)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use bindings::*;

static INIT: Once = Once::new();
static mut SGX_CONTEXT: *mut WamrSgxContext = std::ptr::null_mut();

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SGX initialization failed: {0}")]
    SgxInitFailed(i32),
    
    #[error("Failed to load WASM module: {0}")]
    ModuleLoadFailed(i32),
    
    #[error("Failed to call function: {0}")]
    FunctionCallFailed(i32),
    
    #[error("Memory operation failed: {0}")]
    MemoryOpFailed(i32),
    
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
    
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),
    
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl From<Value> for wamr_sgx_val_t {
    fn from(val: Value) -> Self {
        match val {
            Value::I32(v) => wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: v },
            },
            Value::I64(v) => wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I64,
                value: wamr_sgx_val_t__bindgen_ty_1 { i64_: v },
            },
            Value::F32(v) => wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_F32,
                value: wamr_sgx_val_t__bindgen_ty_1 { f32_: v },
            },
            Value::F64(v) => wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_F64,
                value: wamr_sgx_val_t__bindgen_ty_1 { f64_: v },
            },
        }
    }
}

impl TryFrom<wamr_sgx_val_t> for Value {
    type Error = Error;
    
    fn try_from(val: wamr_sgx_val_t) -> Result<Self, Self::Error> {
        match val.type_ {
            wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32 => Ok(Value::I32(unsafe { val.value.i32_ })),
            wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I64 => Ok(Value::I64(unsafe { val.value.i64_ })),
            wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_F32 => Ok(Value::F32(unsafe { val.value.f32_ })),
            wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_F64 => Ok(Value::F64(unsafe { val.value.f64_ })),
            _ => Err(Error::InvalidArgument("Unsupported value type".to_string())),
        }
    }
}

#[derive(Debug)]
struct WasmModuleInner {
    module: *mut WamrSgxModule,
    instance: *mut WamrSgxInstance,
}

unsafe impl Send for WasmModuleInner {}
unsafe impl Sync for WasmModuleInner {}

impl WasmModuleInner {
    unsafe fn get_instance_context(&self) -> *mut WamrSgxContext {
        if self.instance.is_null() {
            return std::ptr::null_mut();
        }
        
        let context_ptr_addr = self.instance as usize;
        let context_ptr = *(context_ptr_addr as *const *mut WamrSgxContext);
        context_ptr
    }
    
    unsafe fn set_instance_context(&self, ctx: *mut WamrSgxContext) {
        if self.instance.is_null() {
            return;
        }
        
        let context_ptr_addr = self.instance as usize;
        *(context_ptr_addr as *mut *mut WamrSgxContext) = ctx;
    }
    
    unsafe fn get_module_context(&self) -> *mut WamrSgxContext {
        if self.module.is_null() {
            return std::ptr::null_mut();
        }
        
        let context_ptr_addr = self.module as usize;
        let context_ptr = *(context_ptr_addr as *const *mut WamrSgxContext);
        context_ptr
    }
    
    unsafe fn set_module_context(&self, ctx: *mut WamrSgxContext) {
        if self.module.is_null() {
            return;
        }
        
        let context_ptr_addr = self.module as usize;
        *(context_ptr_addr as *mut *mut WamrSgxContext) = ctx;
    }
}

impl Drop for WasmModuleInner {
    fn drop(&mut self) {
        debug!("[SGX Cleanup] Dropping WasmModuleInner, all references are gone");
        unsafe {
            if !self.instance.is_null() {
                debug!("[SGX Cleanup] Destroying WASM instance: {:p}", self.instance);

                let instance_ctx = self.get_instance_context();
                if instance_ctx != SGX_CONTEXT {
                    debug!("[SGX Fix] Fixing instance context mismatch before destroying instance");
                    self.set_instance_context(SGX_CONTEXT);
                }
                
                wamr_sgx_destroy_instance(SGX_CONTEXT, self.instance);
                self.instance = std::ptr::null_mut();
            }
            
            if !self.module.is_null() {
                debug!("[SGX Cleanup] Unloading WASM module: {:p}", self.module);

                let module_ctx = self.get_module_context();
                if module_ctx != SGX_CONTEXT {
                    debug!("[SGX Fix] Fixing module context mismatch before unloading module");
                    self.set_module_context(SGX_CONTEXT);
                }
                
                wamr_sgx_unload_module(SGX_CONTEXT, self.module);
                self.module = std::ptr::null_mut();
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct WasmModule {
    inner: Arc<WasmModuleInner>,
}

unsafe impl Send for WasmModule {}
unsafe impl Sync for WasmModule {}

impl WasmModule {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Result<Self, Error> {
        INIT.call_once(|| {
            unsafe {
                match env::current_dir() {
                    Ok(path) => debug!("[SGX Init] Current working directory: {}", path.display()),
                    Err(e) => error!("[SGX Init] Failed to get current directory: {}", e),
                }
                
                let enclave_path = match env::var("WAMR_ENCLAVE_PATH") {
                    Ok(path) => {
                        debug!("[SGX Init] WAMR_ENCLAVE_PATH environment variable: {}", path);
                        debug!("[SGX Init] Using enclave path from environment: {}", path);
                        
                        if Path::new(&path).exists() {
                            debug!("[SGX Init] Enclave file exists at: {}", path);
                        } else {
                            error!("[SGX Init] WARNING: Enclave file does not exist at: {}", path);
                        }
                        
                        Some(path)
                    },
                    Err(_) => {
                        error!("[SGX Init] WAMR_ENCLAVE_PATH not set, using default paths");
                        None
                    }
                };
                
                if let Some(path) = enclave_path {
                    debug!("[SGX Init] Attempting to load enclave from: {}", path);
                }
                
                let mut ctx = ptr::null_mut();
                let result = wamr_sgx_init(&mut ctx);
                debug!("[SGX Init] sgx_create_enclave returned: {} (0x{:x})", result, result);
                
                if result == 0 {
                    SGX_CONTEXT = ctx;
                    debug!("[SGX Init] Enclave created successfully with ID: {}", ctx as u64);
                } else {
                    error!("[SGX Init] Failed to initialize SGX context: error {} (0x{:x})", result, result);
                }
            }
        });
        
        if unsafe { SGX_CONTEXT.is_null() } {
            error!("[SGX Error] SGX context is null after initialization");
            return Err(Error::SgxInitFailed(-1));
        }
        
        let wasm_bytes = match std::fs::read(wasm_path.as_ref()) {
            Ok(bytes) => {
                bytes
            },
            Err(e) => {
                error!("[SGX Error] Failed to read WASM file: {}", e);
                error!("[SGX Error] File exists check: {}", wasm_path.as_ref().exists());
                return Err(Error::IoError(e));
            }
        };
        
        let mut module = ptr::null_mut();
        let result = unsafe {
            wamr_sgx_load_module(
                SGX_CONTEXT,
                wasm_bytes.as_ptr(),
                wasm_bytes.len(),
                &mut module
            )
        };
        
        if result != 0 {
            let error_msg = unsafe {
                let msg = wamr_sgx_get_error(SGX_CONTEXT);
                if !msg.is_null() {
                    CStr::from_ptr(msg).to_string_lossy().to_string()
                } else {
                    format!("Unknown error: {}", result)
                }
            };

            error!("[SGX Error] Failed to load WASM module: {} (0x{:x})", result, result);
            error!("[SGX Error] Error message: {}", error_msg);
            
            return Err(Error::ModuleLoadFailed(result));
        }
        
        
        let mut instance = ptr::null_mut();
        let stack_size = 32 * 1024 * 1024;
        let heap_size = 64 * 1024 * 1024;
        
        
        let result = unsafe {
            wamr_sgx_instantiate(
                SGX_CONTEXT,
                module,
                stack_size,
                heap_size,
                &mut instance
            )
        };
        
        if result != 0 {
            unsafe {
                wamr_sgx_unload_module(SGX_CONTEXT, module);
            }
            
            let error_msg = unsafe {
                let msg = wamr_sgx_get_error(SGX_CONTEXT);
                if !msg.is_null() {
                    CStr::from_ptr(msg).to_string_lossy().to_string()
                } else {
                    format!("Instantiation failed: {}", result)
                }
            };

            error!("[SGX Error] Failed to instantiate WASM module: {} (0x{:x})", result, result);
            error!("[SGX Error] Error message: {}", error_msg);
            
            return Err(Error::ModuleLoadFailed(result));
        }

        let inner = WasmModuleInner {
            module,
            instance,
        };
        
        Ok(Self { inner: Arc::new(inner) })
    }
    
    pub fn call_function(&self, name: &str, args: &[Value]) -> Result<Vec<Value>, Error> {
        
        let name_c = CString::new(name)
            .map_err(|_| Error::InvalidArgument("Function name contains null bytes".to_string()))?;
        
        let mut wamr_args: Vec<wamr_sgx_val_t> = args.iter()
            .map(|arg| arg.clone().into())
            .collect();
        
        
        let result_count = match name {
            "wbm_free" => 0,
            "wbm_alloc" | "wbm_get_byte" | "wbm_set_byte" | "wbm_memory_copy" => 1,
            _ => 1,
        };
        
        let mut returns = Vec::with_capacity(result_count);
        for _ in 0..result_count {
            returns.push(wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
            });
        }

        unsafe {
            if !self.inner.instance.is_null() {
                let instance_ctx = self.inner.get_instance_context();
                if instance_ctx != SGX_CONTEXT {
                    debug!("[SGX Fix] Fixing instance context mismatch before function call to {}", name);
                    self.inner.set_instance_context(SGX_CONTEXT);
                }
            }
        }
        
        let result = unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.inner.instance,
                name_c.as_ptr(),
                wamr_args.as_ptr(),
                wamr_args.len(),
                returns.as_mut_ptr(),
                returns.len()
            )
        };
        
        if result != 0 {
            let error_msg = unsafe {
                let msg = wamr_sgx_get_error(SGX_CONTEXT);
                if !msg.is_null() {
                    CStr::from_ptr(msg).to_string_lossy().to_string()
                } else {
                    format!("Unknown error: {}", result)
                }
            };

            error!("[SGX Error] Function call failed: {} (0x{:x})", result, result);
            error!("[SGX Error] Error message: {}", error_msg);
            return Err(Error::FunctionCallFailed(result));
        }
        
        let values = returns.into_iter()
            .map(|val| {
                let result = Value::try_from(val);
                result
            })
            .collect::<Result<Vec<_>, _>>()?;
        
        let values = if values.is_empty() && name == "wbm_free" {
            vec![]
        } else if values.is_empty() && name == "wbm_set_byte" {
            vec![Value::I32(0)]
        } else {
            values
        };
        
        
        Ok(values)
    }
    
    pub fn read_memory(&self, offset: u32, size: u32) -> Result<Vec<u8>, Error> {
        
        let mut buffer = vec![0u8; size as usize];
        
        const CHUNK_SIZE: u32 = 1024;
        
        for chunk_start in (0..size).step_by(CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(CHUNK_SIZE, size - chunk_start);
            
            
            if let Ok(_) = self.call_memory_copy_function(
                offset + chunk_start,
                &mut buffer[chunk_start as usize..(chunk_start + chunk_size) as usize]
            ) {
                continue;
            }
            
            self.read_memory_bytes(
                offset + chunk_start,
                chunk_size,
                &mut buffer[chunk_start as usize..(chunk_start + chunk_size) as usize]
            )?;
        }
        
        Ok(buffer)
    }
    
    fn call_memory_copy_function(&self, src_offset: u32, dest_buffer: &mut [u8]) -> Result<(), Error> {
        if dest_buffer.is_empty() {
            return Ok(());
        }
        
        let size = dest_buffer.len() as u32;
        
        if let Ok(()) = self.try_read_buffer(src_offset, dest_buffer) {
            return Ok(());
        }
        
        let alloc_args = vec![Value::I32(size as i32)];
        let alloc_result = match self.call_function("wbm_alloc", &alloc_args) {
            Ok(result) => result,
            Err(_) => return Err(Error::InvalidArgument("wbm_alloc function not available".to_string())),
        };
        
        let buffer_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        let copy_args = vec![
            Value::I32(buffer_ptr as i32),
            Value::I32(src_offset as i32),
            Value::I32(size as i32),
        ];
        
        let copy_result = match self.call_function("wbm_memory_copy", &copy_args) {
            Ok(result) => result,
            Err(e) => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(e);
            }
        };
        
        let status = match &copy_result[0] {
            Value::I32(status) => *status,
            _ => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument("Invalid result from wbm_memory_copy".to_string()));
            }
        };
        
        if status != 0 {
            let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
            let _ = self.call_function("wbm_free", &free_args);
            return Err(Error::MemoryOpFailed(status));
        }
        
        const CHUNK_SIZE: u32 = 128;
        
        for chunk_start in (0..size).step_by(CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(CHUNK_SIZE, size - chunk_start);
            let buffer_offset = chunk_start;
            
            for i in 0..chunk_size {
                let read_args = vec![Value::I32((buffer_ptr + buffer_offset + i) as i32)];
                let read_result = match self.call_function("wbm_get_byte", &read_args) {
                    Ok(result) => result,
                    Err(e) => {
                        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                        let _ = self.call_function("wbm_free", &free_args);
                        return Err(e);
                    }
                };
                
                let byte_value = match &read_result[0] {
                    Value::I32(val) => *val as u8,
                    _ => {
                        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                        let _ = self.call_function("wbm_free", &free_args);
                        return Err(Error::InvalidArgument("Invalid result from wbm_get_byte".to_string()));
                    }
                };
                
                let dest_idx = (buffer_offset + i) as usize;
                if dest_idx < dest_buffer.len() {
                    dest_buffer[dest_idx] = byte_value;
                }
            }
        }
        
        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
        let _ = self.call_function("wbm_free", &free_args);
        
        Ok(())
    }
    
    fn try_read_buffer(&self, src_offset: u32, dest_buffer: &mut [u8]) -> Result<(), Error> {
        if dest_buffer.is_empty() {
            return Ok(());
        }
        
        let size = dest_buffer.len() as u32;
        
        let mut alloc_arg = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: size as i32 },
        };
        
        let mut alloc_return = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
        };
        
        let alloc_name_c = CString::new("wbm_alloc").unwrap();
        let alloc_result = unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.inner.instance,
                alloc_name_c.as_ptr(),
                &mut alloc_arg,
                1,
                &mut alloc_return,
                1
            )
        };
        
        if alloc_result != 0 {
            return Err(Error::InvalidArgument("wbm_alloc function not available".to_string()));
        }
        
        let alloc_result = vec![Value::I32(unsafe { alloc_return.value.i32_ })];
        
        let buffer_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        let read_buffer_args = vec![
            Value::I32(src_offset as i32),
            Value::I32(buffer_ptr as i32),
            Value::I32(size as i32),
        ];
        
        let read_result = match self.call_function("wbm_read_buffer", &read_buffer_args) {
            Ok(result) => result,
            Err(_) => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument("wbm_read_buffer function not available".to_string()));
            }
        };
        
        let status = match &read_result[0] {
            Value::I32(status) => *status,
            _ => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument("Invalid result from wbm_read_buffer".to_string()));
            }
        };
        
        if status != 0 {
            let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
            let _ = self.call_function("wbm_free", &free_args);
            return Err(Error::MemoryOpFailed(status));
        }
        
        const CHUNK_SIZE: u32 = 128;
        
        for chunk_start in (0..size).step_by(CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(CHUNK_SIZE, size - chunk_start);
            
            for i in 0..chunk_size {
                let read_args = vec![Value::I32((buffer_ptr + chunk_start + i) as i32)];
                let read_result = match self.call_function("wbm_get_byte", &read_args) {
                    Ok(result) => result,
                    Err(e) => {
                        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                        let _ = self.call_function("wbm_free", &free_args);
                        return Err(e);
                    }
                };
                
                let byte_value = match &read_result[0] {
                    Value::I32(val) => *val as u8,
                    _ => {
                        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                        let _ = self.call_function("wbm_free", &free_args);
                        return Err(Error::InvalidArgument("Invalid result from wbm_get_byte".to_string()));
                    }
                };
                
                let dest_idx = (chunk_start + i) as usize;
                if dest_idx < dest_buffer.len() {
                    dest_buffer[dest_idx] = byte_value;
                }
            }
        }
        
        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
        let _ = self.call_function("wbm_free", &free_args);
        
        Ok(())
    }
    
    fn read_memory_bytes(&self, offset: u32, size: u32, buffer: &mut [u8]) -> Result<(), Error> {

        let get_byte_name_c = CString::new("wbm_get_byte").unwrap();
        let mut arg_buf  = [wamr_sgx_val_t::default(); 2];
        arg_buf[1].value.i32_ = 0i32;

        unsafe {
            let instance_ctx = self.inner.get_instance_context();
            if !self.inner.instance.is_null() && instance_ctx != SGX_CONTEXT {
                debug!("[SGX Fix] Fixing instance context mismatch before read_memory_bytes");
                self.inner.set_instance_context(SGX_CONTEXT);
            }
        }

        for i in 0..size {
            arg_buf[0].value.i32_ = (offset + i) as i32;

            let read_result = unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.inner.instance,
                    get_byte_name_c.as_ptr(),
                    &mut arg_buf[0],
                    1,
                    &mut arg_buf[1],
                    1
                )
            };
            
            if read_result != 0 {
                return Err(Error::FunctionCallFailed(read_result));
            }
            
            let byte_value = unsafe { arg_buf[1].value.i32_ as u8 };
            
            if i < buffer.len() as u32 {
                buffer[i as usize] = byte_value;
            }
        }
        
        Ok(())
    }
    
    pub fn write_memory(&self, offset: u32, data: &[u8]) -> Result<(), Error> {
        
        if data.is_empty() {
            return Ok(());
        }
        
        const CHUNK_SIZE: usize = 1024;
        
        for chunk_start in (0..data.len()).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, data.len());
            let chunk_size = chunk_end - chunk_start;
            let target_offset = offset + chunk_start as u32;
            
            
            if let Ok(_) = self.call_memory_copy_for_write(
                target_offset, 
                &data[chunk_start..chunk_end]
            ) {
                continue;
            }
            
            for (i, &byte) in data[chunk_start..chunk_end].iter().enumerate() {
                let byte_offset = target_offset + i as u32;
                let set_byte_args = vec![
                    Value::I32(byte as i32),
                    Value::I32(byte_offset as i32),
                ];
                
                let result = self.call_function("wbm_set_byte", &set_byte_args)?;
                if let Value::I32(status) = result[0] {
                    if status != 0 {
                        return Err(Error::MemoryOpFailed(status));
                    }
                } else {
                    return Err(Error::InvalidArgument("Invalid result from wbm_set_byte".to_string()));
                }
            }
        }
        
        Ok(())
    }
    
    fn call_memory_copy_for_write(&self, dest_offset: u32, src_data: &[u8]) -> Result<(), Error> {
        if src_data.is_empty() {
            return Ok(());
        }
        
        let size = src_data.len() as u32;
        
        let alloc_args = vec![Value::I32(size as i32)];
        let alloc_result = match self.call_function("wbm_alloc", &alloc_args) {
            Ok(result) => result,
            Err(_) => return Err(Error::InvalidArgument("wbm_alloc function not available".to_string())),
        };
        
        let buffer_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        for (i, &byte) in src_data.iter().enumerate() {
            let set_byte_args = vec![
                Value::I32(byte as i32),
                Value::I32((buffer_ptr + i as u32) as i32),
            ];
            
            let result = match self.call_function("wbm_set_byte", &set_byte_args) {
                Ok(result) => result,
                Err(e) => {
                    let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                    let _ = self.call_function("wbm_free", &free_args);
                    return Err(e);
                }
            };
            
            if let Value::I32(status) = result[0] {
                if status != 0 {
                    let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                    let _ = self.call_function("wbm_free", &free_args);
                    return Err(Error::MemoryOpFailed(status));
                }
            } else {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument("Invalid result from wbm_set_byte".to_string()));
            }
        }
        
        let copy_args = vec![
            Value::I32(dest_offset as i32),
            Value::I32(buffer_ptr as i32),
            Value::I32(size as i32),
        ];
        
        let copy_result = match self.call_function("wbm_memory_copy", &copy_args) {
            Ok(result) => result,
            Err(e) => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(e);
            }
        };
        
        let status = match &copy_result[0] {
            Value::I32(status) => *status,
            _ => {
                let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
                let _ = self.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument("Invalid result from wbm_memory_copy".to_string()));
            }
        };
        
        let free_args = vec![Value::I32(buffer_ptr as i32), Value::I32(size as i32)];
        let _ = self.call_function("wbm_free", &free_args);
        
        if status != 0 {
            return Err(Error::MemoryOpFailed(status));
        }
        
        Ok(())
    }
}


#[derive(Clone, Debug)]
pub struct BlockBuilderSgx {
    module: WasmModule,
}

impl Default for wamr_sgx_val_t {
    fn default() -> Self {
        Self {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 4096 },
        }
    }
}

impl BlockBuilderSgx {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Result<Self, Error> {
        let module = WasmModule::new(wasm_path)?;
        let module_info = Self::get_module_info_internal(&module)?;
        
        log::info!("SGX WASM module initialized successfully with Arc protection");
        log::debug!("Module info: {}", module_info);
        
        Ok(Self { module })
    }
    fn get_module_info_internal(module: &WasmModule) -> Result<String, Error> {
        let output_size = 4096;
        let alloc_args = vec![Value::I32(output_size as i32)];
        let alloc_result = module.call_function("wbm_alloc", &alloc_args)?;
        
        let output_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        module.write_memory(len_ptr, &(output_size as u32).to_le_bytes())?;
        let get_info_args = vec![
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let info_result = module.call_function("get_module_info", &get_info_args)?;
        
        let result_code = match &info_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument("Invalid result code from get_module_info".to_string()));
            }
        };
        
        if result_code != 0 {
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let _ = module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = module.call_function("wbm_free", &free_len_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        let len_data = module.read_memory(len_ptr, 4)?;
        let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
        let data = module.read_memory(output_ptr, output_len)?;
        let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
        let _ = module.call_function("wbm_free", &free_output_args)?;
        let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
        let _ = module.call_function("wbm_free", &free_len_args)?;
        let info = String::from_utf8(data)
            .map_err(|_| Error::InvalidArgument("Module info is not valid UTF-8".to_string()))?;
        
        Ok(info)
    }
    pub fn get_public_key(&self) -> Result<String, Error> {
        
        let initial_buffer_size = 1024; 
        let alloc_args = vec![Value::I32(initial_buffer_size as i32)];
        let alloc_result = self.module.call_function("wbm_alloc", &alloc_args)?;
        
        let output_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = self.module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_buffer_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        self.module.write_memory(len_ptr, &(initial_buffer_size as u32).to_le_bytes())?;
        
        let get_pk_args = vec![
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let pk_result = self.module.call_function("get_public_key", &get_pk_args)?;
        
        let result_code = match &pk_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_buffer_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument("Invalid result code from get_public_key".to_string()));
            }
        };
        
        if result_code == -2 {
            
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let required_size = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_buffer_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            
            let new_alloc_args = vec![Value::I32(required_size as i32)];
            let new_alloc_result = self.module.call_function("wbm_alloc", &new_alloc_args)?;
            
            let new_output_ptr = match &new_alloc_result[0] {
                Value::I32(ptr) => *ptr as u32,
                _ => {
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                }
            };
            
            self.module.write_memory(len_ptr, &required_size.to_le_bytes())?;
            
            let new_get_pk_args = vec![
                Value::I32(new_output_ptr as i32),
                Value::I32(len_ptr as i32),
            ];
            
            let new_pk_result = self.module.call_function("get_public_key", &new_get_pk_args)?;
            
            let new_result_code = match &new_pk_result[0] {
                Value::I32(code) => *code,
                _ => {
                    let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid result code from get_public_key".to_string()));
                }
            };
            
            if new_result_code != 0 {
                let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args)?;
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args)?;
                
                return Err(Error::FunctionCallFailed(new_result_code));
            }
            
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            let key_data = self.module.read_memory(new_output_ptr, output_len)?;
            
            let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            let key_string = String::from_utf8(key_data)
                .map_err(|_| Error::InvalidArgument("Public key data is not valid UTF-8".to_string()))?;
            
            Ok(key_string)
        } else if result_code != 0 {
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_buffer_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        } else {
            
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            let key_data = self.module.read_memory(output_ptr, output_len)?;
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_buffer_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            let key_string = String::from_utf8(key_data)
                .map_err(|_| Error::InvalidArgument("Public key data is not valid UTF-8".to_string()))?;
            
            Ok(key_string)
        }
    }
    
    pub fn build_block(&self, input_json: &str) -> Result<String, Error> {
        const INPUT_CHUNK_SIZE: usize = 64 * 1024; 
        
        let mut attempt = 0;
        let max_attempts = 1; 
        let mut input_ptr = 0u32;
        
        let max_single_alloc = 256 * 1024; 
        let alloc_size = std::cmp::min(input_json.len(), max_single_alloc);

        debug!("[SGX Memory] Allocating memory for input: {} bytes (input size: {} bytes)", 
                 alloc_size, input_json.len());
        
        while attempt < max_attempts {
            attempt += 1;
            let alloc_args = vec![Value::I32(alloc_size as i32)];
            match self.module.call_function("wbm_alloc", &alloc_args) {
                Ok(result) => {
                    match &result[0] {
                        Value::I32(ptr) => {
                            input_ptr = *ptr as u32;
                            debug!("[SGX Memory] Successfully allocated {} bytes at ptr {}", alloc_size, input_ptr);
                            break;
                        },
                        _ => {
                            if attempt == max_attempts {
                                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                            }
                            debug!("[SGX Memory] Invalid pointer type returned, retrying in {}ms", 200 * attempt);
                            std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
                        }
                    }
                },
                Err(e) => {
                    error!("[SGX Error] Failed to allocate memory for input after {} attempts: {}", attempt, e);
                }
            }
        }
        
        for chunk_start in (0..input_json.len()).step_by(INPUT_CHUNK_SIZE).enumerate() {
            let chunk_end = std::cmp::min(chunk_start.1 + INPUT_CHUNK_SIZE, input_json.len());
            let chunk = &input_json.as_bytes()[chunk_start.1..chunk_end];
            let target_offset = input_ptr + chunk_start.1 as u32;
            
            let mut write_attempt = 0;
            let max_write_attempts = 3;
            let mut write_success = false;
            
            while write_attempt < max_write_attempts && !write_success {
                write_attempt += 1;
                match self.module.write_memory(target_offset, chunk) {
                    Ok(_) => {
                        write_success = true;
                    },
                    Err(e) => {
                        if write_attempt == max_write_attempts {
                            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                            let _ = self.module.call_function("wbm_free", &free_input_args);
                            return Err(e);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        }
        
        let initial_output_size = 128 * 1024; 
        
        attempt = 0;
        let mut output_ptr = 0u32;

        debug!("[SGX Memory] Allocating output buffer: {} bytes", initial_output_size);
        
        while attempt < max_attempts {
            attempt += 1;
            
            if attempt > 2 && alloc_size < input_json.len() {
                warn!("[SGX Memory] Freeing input buffer to reduce memory pressure");
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
            }
            
            let output_alloc_args = vec![Value::I32(initial_output_size as i32)];
            match self.module.call_function("wbm_alloc", &output_alloc_args) {
                Ok(result) => {
                    match &result[0] {
                        Value::I32(ptr) => {
                            output_ptr = *ptr as u32;
                            debug!("[SGX Memory] Successfully allocated output buffer of {} bytes at ptr {}", 
                                    initial_output_size, output_ptr);
                            break;
                        },
                        _ => {
                            if attempt == max_attempts {
                                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                                let _ = self.module.call_function("wbm_free", &free_input_args);
                                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                            }
                            warn!("[SGX Memory] Invalid pointer type returned for output buffer, retrying in {}ms", 200 * attempt);
                            std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
                        }
                    }
                },
                Err(e) => {
                    if attempt == max_attempts {
                        error!("[SGX Error] Failed to allocate output buffer after {} attempts: {}", max_attempts, e);
                        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                        let _ = self.module.call_function("wbm_free", &free_input_args);
                        return Err(e);
                    }
                    warn!("[SGX Memory] Output buffer allocation attempt {} failed: {}, retrying in {}ms", 
                             attempt, e, 300 * attempt);
                    std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
                }
            }
        }
        
        attempt = 0;
        let mut len_ptr = 0u32;
        
        debug!("[SGX Memory] Allocating length buffer: 4 bytes");
        
        while attempt < max_attempts {
            attempt += 1;
            
            let len_alloc_args = vec![Value::I32(8)]; 
            match self.module.call_function("wbm_alloc", &len_alloc_args) {
                Ok(result) => {
                    match &result[0] {
                        Value::I32(ptr) => {
                            len_ptr = *ptr as u32;
                            debug!("[SGX Memory] Successfully allocated length buffer of 8 bytes at ptr {}", len_ptr);
                            break;
                        },
                        _ => {
                            if attempt == max_attempts {
                                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                                let _ = self.module.call_function("wbm_free", &free_input_args);
                                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                                let _ = self.module.call_function("wbm_free", &free_output_args);
                                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                            }
                            warn!("[SGX Memory] Invalid pointer type returned for length buffer, retrying in {}ms", 200 * attempt);
                            std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
                        }
                    }
                },
                Err(e) => {
                    if attempt == max_attempts {
                        error!("[SGX Error] Failed to allocate length buffer after {} attempts: {}", max_attempts, e);
                        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                        let _ = self.module.call_function("wbm_free", &free_input_args);
                        let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                        let _ = self.module.call_function("wbm_free", &free_output_args);
                        return Err(e);
                    }
                    warn!("[SGX Memory] Length buffer allocation attempt {} failed: {}, retrying in {}ms", 
                             attempt, e, 300 * attempt);
                    
                    if attempt > 3 {
                        warn!("[SGX Memory] Freeing input and output buffers to reduce memory pressure");
                        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                        let _ = self.module.call_function("wbm_free", &free_input_args);
                        let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                        let _ = self.module.call_function("wbm_free", &free_output_args);
                        
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300 * attempt as u64));
                }
            }
        }
        
        if let Err(e) = self.module.write_memory(len_ptr, &(initial_output_size as u32).to_le_bytes()) {
            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
            let _ = self.module.call_function("wbm_free", &free_input_args);
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args);
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args);
            return Err(e);
        }
        
        let build_args = vec![
            Value::I32(input_ptr as i32),
            Value::I32(input_json.len() as i32),
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let build_result = match self.module.call_function("build_block", &build_args) {
            Ok(result) => result,
            Err(e) => {
                error!("[SGX Error] build_block function call failed: {}", e);
                
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                
                return Err(e);
            }
        };
        
        let result_code = match &build_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                
                return Err(Error::InvalidArgument("Invalid result code from build_block".to_string()));
            }
        };
        
        let mut actual_output_ptr = output_ptr;
        let mut actual_output_size = initial_output_size as u32;
        
        if result_code == -2 {
            let len_data = match self.module.read_memory(len_ptr, 4) {
                Ok(data) => data,
                Err(e) => {
                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                    let _ = self.module.call_function("wbm_free", &free_input_args);
                    let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(e);
                }
            };
            
            let required_size = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            if required_size > 50 * 1024 * 1024 {  
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument(format!("Required output size too large: {} bytes", required_size)));
            }
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
            if let Err(_) = self.module.call_function("wbm_free", &free_output_args) {
            }
            
            std::thread::sleep(std::time::Duration::from_millis(50));
            
            attempt = 0;
            let mut new_output_ptr = 0u32;
            
            while attempt < max_attempts {
                attempt += 1;
                let new_alloc_args = vec![Value::I32(required_size as i32)];
                match self.module.call_function("wbm_alloc", &new_alloc_args) {
                    Ok(result) => {
                        match &result[0] {
                            Value::I32(ptr) => {
                                new_output_ptr = *ptr as u32;
                                break;
                            },
                            _ => {
                                if attempt == max_attempts {
                                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                                    let _ = self.module.call_function("wbm_free", &free_input_args);
                                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                                    let _ = self.module.call_function("wbm_free", &free_len_args);
                                    return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                                }
                                std::thread::sleep(std::time::Duration::from_millis(100 * attempt)); 
                            }
                        }
                    },
                    Err(e) => {
                        if attempt == max_attempts {
                            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                            let _ = self.module.call_function("wbm_free", &free_input_args);
                            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                            let _ = self.module.call_function("wbm_free", &free_len_args);
                            error!("[SGX Error] Failed to allocate new output buffer: {}", e);
                            return Err(e);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100 * attempt)); 
                    }
                }
            }
            
            if let Err(e) = self.module.write_memory(len_ptr, &required_size.to_le_bytes()) {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(e);
            }
            
            let new_build_args = vec![
                Value::I32(input_ptr as i32), 
                Value::I32(input_json.len() as i32),
                Value::I32(new_output_ptr as i32),
                Value::I32(len_ptr as i32),
            ];
            
            let new_build_result = match self.module.call_function("build_block", &new_build_args) {
                Ok(result) => result,
                Err(e) => {
                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                    let _ = self.module.call_function("wbm_free", &free_input_args);
                    let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(e);
                }
            };
            
            let new_result_code = match &new_build_result[0] {
                Value::I32(code) => *code,
                _ => {
                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                    let _ = self.module.call_function("wbm_free", &free_input_args);
                    let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid result code from second build_block".to_string()));
                }
            };
            
            if new_result_code != 0 {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                
                return Err(Error::FunctionCallFailed(new_result_code));
            }
            
            actual_output_ptr = new_output_ptr;
            actual_output_size = required_size;
        } else if result_code != 0 {
            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
            let _ = self.module.call_function("wbm_free", &free_input_args);
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args);
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args);
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        
        let len_data = match self.module.read_memory(len_ptr, 4) {
            Ok(data) => data,
            Err(e) => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(actual_output_ptr as i32), Value::I32(actual_output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(e);
            }
        };
        
        let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
        
        if output_len > actual_output_size {
            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
            let _ = self.module.call_function("wbm_free", &free_input_args);
            let free_output_args = vec![Value::I32(actual_output_ptr as i32), Value::I32(actual_output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args);
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args);
            return Err(Error::InvalidArgument(format!("Output length ({}) exceeds buffer size ({})", output_len, actual_output_size)));
        }
        
        const OUTPUT_CHUNK_SIZE: u32 = 64 * 1024; 
        let mut output_data = Vec::with_capacity(output_len as usize);
        
        for chunk_start in (0..output_len).step_by(OUTPUT_CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(OUTPUT_CHUNK_SIZE, output_len - chunk_start);
            
            let mut read_attempt = 0;
            let max_read_attempts = 3;
            let mut chunk_data = Vec::new();
            
            while read_attempt < max_read_attempts {
                read_attempt += 1;
                match self.module.read_memory(actual_output_ptr + chunk_start, chunk_size) {
                    Ok(data) => {
                        chunk_data = data;
                        break;
                    },
                    Err(e) => {
                        if read_attempt == max_read_attempts {
                            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                            let _ = self.module.call_function("wbm_free", &free_input_args);
                            let free_output_args = vec![Value::I32(actual_output_ptr as i32), Value::I32(actual_output_size as i32)];
                            let _ = self.module.call_function("wbm_free", &free_output_args);
                            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                            let _ = self.module.call_function("wbm_free", &free_len_args);
                            return Err(e);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
            
            output_data.extend_from_slice(&chunk_data);
        }

        debug!("[SGX Memory] Cleaning up resources");
        
        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
        if let Err(e) = self.module.call_function("wbm_free", &free_input_args) {
            error!("[SGX Warning] Failed to free input buffer: {}", e);
        } else {
            debug!("[SGX Memory] Successfully freed input buffer of {} bytes at ptr {}", 
                    alloc_size, input_ptr);
        }
        
        let free_output_args = vec![Value::I32(actual_output_ptr as i32), Value::I32(actual_output_size as i32)];
        if let Err(e) = self.module.call_function("wbm_free", &free_output_args) {
            error!("[SGX Warning] Failed to free output buffer: {}", e);
        } else {
            debug!("[SGX Memory] Successfully freed output buffer of {} bytes at ptr {}", 
                    actual_output_size, actual_output_ptr);
        }
        
        let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(8)]; 
        if let Err(e) = self.module.call_function("wbm_free", &free_len_args) {
            error!("[SGX Warning] Failed to free length buffer: {}", e);
        } else {
            debug!("[SGX Memory] Successfully freed length buffer of 8 bytes at ptr {}", len_ptr);
        }
        
        let output_str = match String::from_utf8(output_data) {
            Ok(str) => str,
            Err(_) => return Err(Error::InvalidArgument("Output is not valid UTF-8".to_string())),
        };
        
        Ok(output_str)
    }
    
    pub fn order_transactions(&self, input_json: &str) -> Result<String, Error> {
        const INPUT_CHUNK_SIZE: usize = 64 * 1024; 
        
        let mut attempt = 0;
        let max_attempts = 1; 
        let mut input_ptr = 0u32;
        
        let max_single_alloc = 256 * 1024; 
        let alloc_size = std::cmp::min(input_json.len(), max_single_alloc);

        debug!("[SGX Memory] Allocating memory for ordering input: {} bytes (input size: {} bytes)", 
                 alloc_size, input_json.len());
        
        while attempt < max_attempts {
            attempt += 1;
            let alloc_args = vec![Value::I32(alloc_size as i32)];
            match self.module.call_function("wbm_alloc", &alloc_args) {
                Ok(result) => {
                    match &result[0] {
                        Value::I32(ptr) => {
                            input_ptr = *ptr as u32;
                            debug!("[SGX Memory] Successfully allocated {} bytes at ptr {}", alloc_size, input_ptr);
                            break;
                        },
                        _ => {
                            if attempt == max_attempts {
                                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                            }
                            debug!("[SGX Memory] Invalid pointer type returned, retrying in {}ms", 200 * attempt);
                            std::thread::sleep(std::time::Duration::from_millis(200 * attempt as u64));
                        }
                    }
                },
                Err(e) => {
                    error!("[SGX Error] Failed to allocate memory for ordering input after {} attempts: {}", attempt, e);
                }
            }
        }
        
        for chunk_start in (0..input_json.len()).step_by(INPUT_CHUNK_SIZE).enumerate() {
            let chunk_end = std::cmp::min(chunk_start.1 + INPUT_CHUNK_SIZE, input_json.len());
            let chunk = &input_json.as_bytes()[chunk_start.1..chunk_end];
            let target_offset = input_ptr + chunk_start.1 as u32;
            
            let mut write_attempt = 0;
            let max_write_attempts = 3;
            let mut write_success = false;
            
            while write_attempt < max_write_attempts && !write_success {
                write_attempt += 1;
                match self.module.write_memory(target_offset, chunk) {
                    Ok(_) => {
                        write_success = true;
                        debug!("[SGX Memory] Successfully wrote chunk {} ({} bytes) at offset {}", chunk_start.0, chunk.len(), target_offset);
                    },
                    Err(e) => {
                        debug!("[SGX Memory] Failed to write chunk {}, attempt {} of {}: {}", chunk_start.0, write_attempt, max_write_attempts, e);
                        if write_attempt < max_write_attempts {
                            std::thread::sleep(std::time::Duration::from_millis(100 * write_attempt as u64));
                        }
                    }
                }
            }
            
            if !write_success {
                let free_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_args);
                return Err(Error::InvalidArgument(format!("Failed to write chunk {} after {} attempts", chunk_start.0, max_write_attempts)));
            }
        }
        
        let output_size = 1024 * 1024; 
        let output_alloc_args = vec![Value::I32(output_size as i32)];
        let output_alloc_result = self.module.call_function("wbm_alloc", &output_alloc_args)?;
        
        let output_ptr = match &output_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc for output".to_string()));
            }
        };
        
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = self.module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let _ = self.module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc for length".to_string()));
            }
        };
        
        self.module.write_memory(len_ptr, &(output_size as u32).to_le_bytes())?;
        
        let order_args = vec![
            Value::I32(input_ptr as i32),
            Value::I32(input_json.len() as i32),
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        debug!("[SGX Call] Calling order_transactions with input_ptr={}, input_len={}, output_ptr={}, len_ptr={}", 
               input_ptr, input_json.len(), output_ptr, len_ptr);
        
        let order_result = match self.module.call_function("order_transactions", &order_args) {
            Ok(result) => result,
            Err(e) => {
                error!("[SGX Error] order_transactions function call failed: {}", e);
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument(format!("order_transactions call failed: {}", e)));
            }
        };
        
        let result_code = match &order_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument("Invalid result code from order_transactions".to_string()));
            }
        };
        
        if result_code != 0 {
            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_input_args);
            let _ = self.module.call_function("wbm_free", &free_output_args);
            let _ = self.module.call_function("wbm_free", &free_len_args);
            return Err(Error::InvalidArgument(format!("order_transactions failed with code: {}", result_code)));
        }
        
        let len_data = self.module.read_memory(len_ptr, 4)?;
        let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]) as usize;
        
        debug!("[SGX Memory] Reading ordering output: {} bytes from ptr {}", output_len, output_ptr);
        
        let output_data = self.module.read_memory(output_ptr, output_len as u32)?;
        
        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(alloc_size as i32)];
        let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
        let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
        let _ = self.module.call_function("wbm_free", &free_input_args);
        let _ = self.module.call_function("wbm_free", &free_output_args);
        let _ = self.module.call_function("wbm_free", &free_len_args);
        
        let output_str = match String::from_utf8(output_data) {
            Ok(str) => str,
            Err(_) => return Err(Error::InvalidArgument("Ordering output is not valid UTF-8".to_string())),
        };
        
        debug!("[SGX Success] order_transactions completed successfully, output length: {}", output_str.len());
        Ok(output_str)
    }
    
    pub fn get_module_info(&self) -> Result<String, Error> {
        
        let output_size = 4096;
        let alloc_args = vec![Value::I32(output_size as i32)];
        let alloc_result = self.module.call_function("wbm_alloc", &alloc_args)?;
        
        let output_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = self.module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        self.module.write_memory(len_ptr, &(output_size as u32).to_le_bytes())?;
        
        let get_info_args = vec![
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let info_result = self.module.call_function("get_module_info", &get_info_args)?;
        
        let result_code = match &info_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument("Invalid result code from get_module_info".to_string()));
            }
        };
        
        if result_code != 0 {
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args);
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args);
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        
        let len_data = self.module.read_memory(len_ptr, 4)?;
        let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
        
        let chunk_size = 1024; 
        let mut output_data = Vec::with_capacity(output_len as usize);
        
        for chunk_start in (0..output_len).step_by(chunk_size as usize) {
            let chunk_size = std::cmp::min(chunk_size, output_len - chunk_start);
            
            let chunk_data = self.module.read_memory(output_ptr + chunk_start, chunk_size)?;
            output_data.extend_from_slice(&chunk_data);
        }
        
        let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
        let _ = self.module.call_function("wbm_free", &free_output_args);
        
        let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
        let _ = self.module.call_function("wbm_free", &free_len_args);
        
        let output_str = String::from_utf8(output_data)
            .map_err(|_| Error::InvalidArgument("Module info data is not valid UTF-8".to_string()))?;
        
        Ok(output_str)
    }
    
    pub fn estimate_gas(&self, tx_json: &str, state_json: &str) -> Result<u64, Error> {
        
        const CHUNK_SIZE: usize = 32 * 1024;
        
        let tx_alloc_args = vec![Value::I32(tx_json.len() as i32)];
        let tx_alloc_result = self.module.call_function("wbm_alloc", &tx_alloc_args)?;
        
        let tx_ptr = match &tx_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        for chunk_start in (0..tx_json.len()).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, tx_json.len());
            let chunk = &tx_json.as_bytes()[chunk_start..chunk_end];
            let target_offset = tx_ptr + chunk_start as u32;
            
            self.module.write_memory(target_offset, chunk)?;
        }
        
        let state_alloc_args = vec![Value::I32(state_json.len() as i32)];
        let state_alloc_result = self.module.call_function("wbm_alloc", &state_alloc_args)?;
        
        let state_ptr = match &state_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_tx_args = vec![Value::I32(tx_ptr as i32), Value::I32(tx_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_tx_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        for chunk_start in (0..state_json.len()).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, state_json.len());
            let chunk = &state_json.as_bytes()[chunk_start..chunk_end];
            let target_offset = state_ptr + chunk_start as u32;
            
            self.module.write_memory(target_offset, chunk)?;
        }
        
        let result_alloc_args = vec![Value::I32(8)];
        let result_alloc_result = self.module.call_function("wbm_alloc", &result_alloc_args)?;
        
        let result_ptr = match &result_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_tx_args = vec![Value::I32(tx_ptr as i32), Value::I32(tx_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_tx_args);
                let free_state_args = vec![Value::I32(state_ptr as i32), Value::I32(state_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_state_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        let args = vec![
            Value::I32(tx_ptr as i32),
            Value::I32(tx_json.len() as i32),
            Value::I32(state_ptr as i32),
            Value::I32(state_json.len() as i32),
            Value::I32(result_ptr as i32),
        ];
        
        let estimate_result = self.module.call_function("estimate_gas", &args)?;
        
        let result_code = match &estimate_result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_tx_args = vec![Value::I32(tx_ptr as i32), Value::I32(tx_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_tx_args);
                let free_state_args = vec![Value::I32(state_ptr as i32), Value::I32(state_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_state_args);
                let free_result_args = vec![Value::I32(result_ptr as i32), Value::I32(8)];
                let _ = self.module.call_function("wbm_free", &free_result_args);
                return Err(Error::InvalidArgument("Invalid result code from estimate_gas".to_string()));
            }
        };
        
        let free_tx_args = vec![Value::I32(tx_ptr as i32), Value::I32(tx_json.len() as i32)];
        let _ = self.module.call_function("wbm_free", &free_tx_args)?;
        
        let free_state_args = vec![Value::I32(state_ptr as i32), Value::I32(state_json.len() as i32)];
        let _ = self.module.call_function("wbm_free", &free_state_args)?;
        
        if result_code != 0 {
            let free_result_args = vec![Value::I32(result_ptr as i32), Value::I32(8)];
            let _ = self.module.call_function("wbm_free", &free_result_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        
        let result_data = self.module.read_memory(result_ptr, 8)?;
        let gas_estimate = u64::from_le_bytes([
            result_data[0], result_data[1], result_data[2], result_data[3],
            result_data[4], result_data[5], result_data[6], result_data[7]
        ]);
        
        let free_result_args = vec![Value::I32(result_ptr as i32), Value::I32(8)];
        let _ = self.module.call_function("wbm_free", &free_result_args)?;
        
        Ok(gas_estimate)
    }
    
    pub fn get_state_diff_chunk(&self, chunk_id: u32, build_id: &str) -> Result<String, Error> {
        
        let build_id_c = CString::new(build_id)
            .map_err(|_| Error::InvalidArgument("Build ID contains null bytes".to_string()))?;
        
        let output_size = 4 * 1024 * 1024; 
        let alloc_args = vec![Value::I32(output_size as i32)];
        let alloc_result = self.module.call_function("wbm_alloc", &alloc_args)?;
        
        let output_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = self.module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        self.module.write_memory(len_ptr, &(output_size as u32).to_le_bytes())?;
        
        let args = vec![
            Value::I32(chunk_id as i32),
            Value::I32(build_id_c.as_ptr() as i32),
            Value::I32(build_id.len() as i32),
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let result = self.module.call_function("get_state_diff_chunk", &args)?;
        
        let result_code = match &result[0] {
            Value::I32(code) => *code,
            _ => {
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args);
                return Err(Error::InvalidArgument("Invalid result code from get_state_diff_chunk".to_string()));
            }
        };
        
        if result_code == -2 {
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let required_size = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            
            let new_alloc_args = vec![Value::I32(required_size as i32)];
            let new_alloc_result = self.module.call_function("wbm_alloc", &new_alloc_args)?;
            
            let new_output_ptr = match &new_alloc_result[0] {
                Value::I32(ptr) => *ptr as u32,
                _ => {
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                }
            };
            
            self.module.write_memory(len_ptr, &required_size.to_le_bytes())?;
            
            let new_args = vec![
                Value::I32(chunk_id as i32),
                Value::I32(build_id_c.as_ptr() as i32),
                Value::I32(build_id.len() as i32),
                Value::I32(new_output_ptr as i32),
                Value::I32(len_ptr as i32),
            ];
            
            let new_result = self.module.call_function("get_state_diff_chunk", &new_args)?;
            
            let new_result_code = match &new_result[0] {
                Value::I32(code) => *code,
                _ => {
                    let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid result code from get_state_diff_chunk".to_string()));
                }
            };
            
            if new_result_code != 0 {
                let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args)?;
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args)?;
                
                return Err(Error::FunctionCallFailed(new_result_code));
            }
            
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let actual_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            let data = self.module.read_memory(new_output_ptr, actual_len)?;
            
            let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            let chunk_str = String::from_utf8(data)
                .map_err(|_| Error::InvalidArgument("Chunk data is not valid UTF-8".to_string()))?;
            
            Ok(chunk_str)
        } else if result_code != 0 {
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        } else {
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let actual_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            let data = self.module.read_memory(output_ptr, actual_len)?;
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            let chunk_str = String::from_utf8(data)
                .map_err(|_| Error::InvalidArgument("Chunk data is not valid UTF-8".to_string()))?;
            
            Ok(chunk_str)
        }
    }
}