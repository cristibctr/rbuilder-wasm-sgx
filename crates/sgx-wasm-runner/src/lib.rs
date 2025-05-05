
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::io;
use std::ptr;
use std::env;

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

pub struct WasmModule {
    module: *mut WamrSgxModule,
    instance: *mut WamrSgxInstance,
}

unsafe impl Send for WasmModule {}
unsafe impl Sync for WasmModule {}

impl WasmModule {
    pub fn new<P: AsRef<Path>>(wasm_path: P) -> Result<Self, Error> {
        INIT.call_once(|| {
            unsafe {
                match env::current_dir() {
                    Ok(path) => println!("[SGX Init] Current working directory: {}", path.display()),
                    Err(e) => println!("[SGX Init] Failed to get current directory: {}", e),
                }
                
                let enclave_path = match env::var("WAMR_ENCLAVE_PATH") {
                    Ok(path) => {
                        println!("[SGX Init] WAMR_ENCLAVE_PATH environment variable: {}", path);
                        println!("[SGX Init] Using enclave path from environment: {}", path);
                        
                        if Path::new(&path).exists() {
                            println!("[SGX Init] Enclave file exists at: {}", path);
                        } else {
                            println!("[SGX Init] WARNING: Enclave file does not exist at: {}", path);
                        }
                        
                        Some(path)
                    },
                    Err(_) => {
                        println!("[SGX Init] WAMR_ENCLAVE_PATH not set, using default paths");
                        None
                    }
                };
                
                if let Some(path) = enclave_path {
                    println!("[SGX Init] Attempting to load enclave from: {}", path);
                }
                
                let mut ctx = ptr::null_mut();
                let result = wamr_sgx_init(&mut ctx);
                println!("[SGX Init] sgx_create_enclave returned: {} (0x{:x})", result, result);
                
                if result == 0 {
                    SGX_CONTEXT = ctx;
                    println!("[SGX Init] Enclave created successfully with ID: {}", ctx as u64);
                } else {
                    eprintln!("[SGX Init] Failed to initialize SGX context: error {} (0x{:x})", result, result);
                }
            }
        });
        
        if unsafe { SGX_CONTEXT.is_null() } {
            println!("[SGX Error] SGX context is null after initialization");
            return Err(Error::SgxInitFailed(-1));
        } else {
            println!("[SGX] SGX context initialized successfully: {:p}", unsafe { SGX_CONTEXT });
        }
        
        println!("[SGX] Reading WASM file from: {}", wasm_path.as_ref().display());
        let wasm_bytes = match std::fs::read(wasm_path.as_ref()) {
            Ok(bytes) => {
                println!("[SGX] WASM file read successfully, size: {} bytes", bytes.len());
                bytes
            },
            Err(e) => {
                println!("[SGX Error] Failed to read WASM file: {}", e);
                return Err(Error::IoError(e));
            }
        };
        
        let mut error_buf = vec![0u8; 256];
        
        println!("[SGX] Loading WASM module into SGX enclave...");
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
            
            println!("[SGX Error] Failed to load WASM module: {} (0x{:x})", result, result);
            println!("[SGX Error] Error message: {}", error_msg);
            
            return Err(Error::ModuleLoadFailed(result));
        }
        
        println!("[SGX] WASM module loaded successfully: {:p}", module);
        
        println!("[SGX] Creating WASM instance...");
        let mut instance = ptr::null_mut();
        let stack_size = 32 * 1024 * 1024;
        let heap_size = 64 * 1024 * 1024;
        
        println!("[SGX] Instantiating with stack size: {}MB, heap size: {}MB", 
            stack_size / (1024 * 1024),
            heap_size / (1024 * 1024));
        
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
            
            println!("[SGX Error] Failed to instantiate WASM module: {} (0x{:x})", result, result);
            println!("[SGX Error] Error message: {}", error_msg);
            
            return Err(Error::ModuleLoadFailed(result));
        }
        
        println!("[SGX] WASM instance created successfully: {:p}", instance);
        
        Ok(Self { module, instance })
    }
    
    pub fn call_function(&self, name: &str, args: &[Value]) -> Result<Vec<Value>, Error> {
        println!("[SGX] Calling function: {} with {} arguments", name, args.len());
        
        let name_c = CString::new(name)
            .map_err(|_| Error::InvalidArgument("Function name contains null bytes".to_string()))?;
        
        let mut wamr_args: Vec<wamr_sgx_val_t> = args.iter()
            .map(|arg| arg.clone().into())
            .collect();
        
        for (i, arg) in args.iter().enumerate() {
            match arg {
                Value::I32(v) => println!("[SGX] Arg {}: I32({})", i, v),
                Value::I64(v) => println!("[SGX] Arg {}: I64({})", i, v),
                Value::F32(v) => println!("[SGX] Arg {}: F32({})", i, v),
                Value::F64(v) => println!("[SGX] Arg {}: F64({})", i, v),
            }
        }
        
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
        
        println!("[SGX] Calling function with {} expected return values", returns.len());
        
        let result = unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.instance,
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
            
            println!("[SGX Error] Function call failed: {} (0x{:x})", result, result);
            println!("[SGX Error] Error message: {}", error_msg);
            return Err(Error::FunctionCallFailed(result));
        }
        
        println!("[SGX] Function call succeeded, processing {} return values", returns.len());
        let values = returns.into_iter()
            .map(|val| {
                let result = Value::try_from(val);
                match &result {
                    Ok(Value::I32(v)) => println!("[SGX] Return: I32({})", v),
                    Ok(Value::I64(v)) => println!("[SGX] Return: I64({})", v),
                    Ok(Value::F32(v)) => println!("[SGX] Return: F32({})", v),
                    Ok(Value::F64(v)) => println!("[SGX] Return: F64({})", v),
                    Err(e) => println!("[SGX Error] Failed to convert return value: {}", e),
                }
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
        
        println!("[SGX] Function {} returned successfully with {} values", name, values.len());
        
        Ok(values)
    }
    
    pub fn read_memory(&self, offset: u32, size: u32) -> Result<Vec<u8>, Error> {
        println!("[SGX] Reading memory at offset {} with size {}", offset, size);
        
        let mut buffer = vec![0u8; size as usize];
        
        const CHUNK_SIZE: u32 = 1024;
        
        for chunk_start in (0..size).step_by(CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(CHUNK_SIZE, size - chunk_start);
            
            println!("[SGX] Processing chunk at offset {} with size {}", offset + chunk_start, chunk_size);
            
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
        
        println!("[SGX] Successfully read {} bytes from memory", buffer.len());
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
                self.instance,
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

        for i in 0..size {
            arg_buf[0].value.i32_ = (offset + i) as i32;

            let read_result = unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.instance,
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
        println!("[SGX] Writing {} bytes to memory at offset {}", data.len(), offset);
        
        if data.is_empty() {
            println!("[SGX] Empty data buffer, nothing to write");
            return Ok(());
        }
        
        const CHUNK_SIZE: usize = 1024;
        
        for chunk_start in (0..data.len()).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, data.len());
            let chunk_size = chunk_end - chunk_start;
            let target_offset = offset + chunk_start as u32;
            
            println!("[SGX] Writing chunk of {} bytes at offset {}", chunk_size, target_offset);
            
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
        
        println!("[SGX] Successfully wrote {} bytes to memory at offset {}", data.len(), offset);
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

impl Drop for WasmModule {
    fn drop(&mut self) {
        unsafe {
            if !self.instance.is_null() {
                wamr_sgx_destroy_instance(SGX_CONTEXT, self.instance);
                self.instance = ptr::null_mut();
            }
            
            if !self.module.is_null() {
                wamr_sgx_unload_module(SGX_CONTEXT, self.module);
                self.module = ptr::null_mut();
            }
        }
    }
}

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
        Ok(Self { module })
    }
    
    pub fn build_block(&self, input_json: &str) -> Result<String, Error> {
        println!("[SGX] Starting block building with input size: {} bytes", input_json.len());
        
        const CHUNK_SIZE: usize = 32 * 1024;
        
        let alloc_args = vec![Value::I32(input_json.len() as i32)];
        println!("[SGX] Allocating {} bytes for input JSON", input_json.len());
        let alloc_result = self.module.call_function("wbm_alloc", &alloc_args)?;
        
        let input_ptr = match &alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string())),
        };
        
        println!("[SGX] Writing input data in chunks");
        for chunk_start in (0..input_json.len()).step_by(CHUNK_SIZE) {
            let chunk_end = std::cmp::min(chunk_start + CHUNK_SIZE, input_json.len());
            let chunk = &input_json.as_bytes()[chunk_start..chunk_end];
            let target_offset = input_ptr + chunk_start as u32;
            
            self.module.write_memory(target_offset, chunk)?;
        }
        
        let initial_output_size = 128 * 1024;
        println!("[SGX] Allocating initial output buffer of {} bytes", initial_output_size);
        let output_alloc_args = vec![Value::I32(initial_output_size as i32)];
        let output_alloc_result = self.module.call_function("wbm_alloc", &output_alloc_args)?;
        
        let output_ptr = match &output_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        let len_alloc_args = vec![Value::I32(4)];
        let len_alloc_result = self.module.call_function("wbm_alloc", &len_alloc_args)?;
        
        let len_ptr = match &len_alloc_result[0] {
            Value::I32(ptr) => *ptr as u32,
            _ => {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args);
                let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args);
                return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
            }
        };
        
        self.module.write_memory(len_ptr, &(initial_output_size as u32).to_le_bytes())?;
        
        println!("[SGX] Calling build_block function");
        let build_args = vec![
            Value::I32(input_ptr as i32),
            Value::I32(input_json.len() as i32),
            Value::I32(output_ptr as i32),
            Value::I32(len_ptr as i32),
        ];
        
        let build_result = self.module.call_function("build_block", &build_args)?;
        
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
            println!("[SGX] Initial output buffer too small, resizing");
            let len_data = self.module.read_memory(len_ptr, 4)?;
            let required_size = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
            
            println!("[SGX] Required output size: {} bytes", required_size);
            
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            
            let new_alloc_args = vec![Value::I32(required_size as i32)];
            let new_alloc_result = self.module.call_function("wbm_alloc", &new_alloc_args)?;
            
            let new_output_ptr = match &new_alloc_result[0] {
                Value::I32(ptr) => *ptr as u32,
                _ => {
                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                    let _ = self.module.call_function("wbm_free", &free_input_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid pointer returned from wbm_alloc".to_string()));
                }
            };
            
            self.module.write_memory(len_ptr, &required_size.to_le_bytes())?;
            
            println!("[SGX] Calling build_block again with larger buffer");
            let new_build_args = vec![
                Value::I32(input_ptr as i32), 
                Value::I32(input_json.len() as i32),
                Value::I32(new_output_ptr as i32),
                Value::I32(len_ptr as i32),
            ];
            
            let new_build_result = self.module.call_function("build_block", &new_build_args)?;
            
            let new_result_code = match &new_build_result[0] {
                Value::I32(code) => *code,
                _ => {
                    let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                    let _ = self.module.call_function("wbm_free", &free_input_args);
                    let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                    let _ = self.module.call_function("wbm_free", &free_output_args);
                    let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                    let _ = self.module.call_function("wbm_free", &free_len_args);
                    return Err(Error::InvalidArgument("Invalid result code from build_block".to_string()));
                }
            };
            
            if new_result_code != 0 {
                let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
                let _ = self.module.call_function("wbm_free", &free_input_args)?;
                let free_output_args = vec![Value::I32(new_output_ptr as i32), Value::I32(required_size as i32)];
                let _ = self.module.call_function("wbm_free", &free_output_args)?;
                let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
                let _ = self.module.call_function("wbm_free", &free_len_args)?;
                
                return Err(Error::FunctionCallFailed(new_result_code));
            }
            
            actual_output_ptr = new_output_ptr;
            actual_output_size = required_size;
        } else if result_code != 0 {
            let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
            let _ = self.module.call_function("wbm_free", &free_input_args)?;
            let free_output_args = vec![Value::I32(output_ptr as i32), Value::I32(initial_output_size as i32)];
            let _ = self.module.call_function("wbm_free", &free_output_args)?;
            let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
            let _ = self.module.call_function("wbm_free", &free_len_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        
        let len_data = self.module.read_memory(len_ptr, 4)?;
        let output_len = u32::from_le_bytes([len_data[0], len_data[1], len_data[2], len_data[3]]);
        
        println!("[SGX] Reading output data of size: {} bytes", output_len);
        
        let mut output_data = Vec::with_capacity(output_len as usize);
        const READ_CHUNK_SIZE: u32 = 32 * 1024;
        
        for chunk_start in (0..output_len).step_by(READ_CHUNK_SIZE as usize) {
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, output_len - chunk_start);
            let chunk_data = self.module.read_memory(actual_output_ptr + chunk_start, chunk_size)?;
            output_data.extend_from_slice(&chunk_data);
        }
        
        println!("[SGX] Freeing allocated memory");
        let free_input_args = vec![Value::I32(input_ptr as i32), Value::I32(input_json.len() as i32)];
        let _ = self.module.call_function("wbm_free", &free_input_args)?;
        
        let free_output_args = vec![Value::I32(actual_output_ptr as i32), Value::I32(actual_output_size as i32)];
        let _ = self.module.call_function("wbm_free", &free_output_args)?;
        
        let free_len_args = vec![Value::I32(len_ptr as i32), Value::I32(4)];
        let _ = self.module.call_function("wbm_free", &free_len_args)?;
        
        println!("[SGX] Converting output to UTF-8 string");
        let output_str = String::from_utf8(output_data)
            .map_err(|_| Error::InvalidArgument("Output is not valid UTF-8".to_string()))?;
        
        println!("[SGX] Block building completed successfully");
        Ok(output_str)
    }
    
    pub fn get_module_info(&self) -> Result<String, Error> {
        println!("Getting module info from SGX enclave...");
        
        println!("[SGX] Getting module info - Step 1: Function entry");

        let output_size = 4096;
        println!("[SGX] Getting module info - Step 2: Preparing allocation arguments");
        
        let mut output_alloc_arg = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_size },
        };
        
        let mut output_alloc_return = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
        };
        
        let alloc_name_c = CString::new("wbm_alloc").unwrap();
        println!("[SGX] Getting module info - Step 3: Calling wbm_alloc for output buffer");
        
        unsafe {
            println!("[SGX Debug] SGX_CONTEXT: {:p}, module instance: {:p}", SGX_CONTEXT, self.module.instance);
            println!("[SGX Debug] Function args size: output_alloc_arg={} bytes, output_alloc_return={} bytes", 
                     std::mem::size_of_val(&output_alloc_arg), 
                     std::mem::size_of_val(&output_alloc_return));
            println!("[SGX Debug] Allocated pointers: alloc_name_c={:p}", alloc_name_c.as_ptr());
            
            let stack_var_address = &output_size as *const _ as usize;
            println!("[SGX Debug] Local variable address (stack indicator): 0x{:x}", stack_var_address);
        }
        
        let output_alloc_result = unsafe {
            println!("[SGX Debug] wamr_sgx_init @ {:p}", wamr_sgx_init as *const ());
            println!("[SGX Debug] wamr_sgx_load_module @ {:p}", wamr_sgx_load_module as *const ());
            println!("[SGX Debug] wamr_sgx_instantiate @ {:p}", wamr_sgx_instantiate as *const ());
            println!("[SGX Debug] wamr_sgx_call_function @ {:p}", wamr_sgx_call_function as *const ());
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.module.instance,
                alloc_name_c.as_ptr(),
                &mut output_alloc_arg,
                1,
                &mut output_alloc_return,
                1
            )
        };
        
        if output_alloc_result != 0 {
            println!("[SGX Error] Failed to allocate output buffer: {}", output_alloc_result);
            return Err(Error::FunctionCallFailed(output_alloc_result));
        }
        
        let output_ptr = unsafe { output_alloc_return.value.i32_ as u32 };
        println!("[SGX] Getting module info - Step 4: Got output buffer at address {}", output_ptr);
        
        let mut len_alloc_arg = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 4 },
        };
        
        let mut len_alloc_return = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
        };
        
        println!("[SGX] Getting module info - Step 5: Calling wbm_alloc for length buffer");
        let len_alloc_result = unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.module.instance,
                alloc_name_c.as_ptr(),
                &mut len_alloc_arg,
                1,
                &mut len_alloc_return,
                1
            )
        };
        
        if len_alloc_result != 0 {
            let mut free_output_arg1 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_ptr as i32 },
            };
            
            let mut free_output_arg2 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_size },
            };
            
            let free_name_c = CString::new("wbm_free").unwrap();
            unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    free_name_c.as_ptr(),
                    &mut free_output_arg1,
                    1,
                    std::ptr::null_mut(),
                    0
                )
            };
            
            return Err(Error::FunctionCallFailed(len_alloc_result));
        }
        
        let len_ptr = unsafe { len_alloc_return.value.i32_ as u32 };
        println!("[SGX] Getting module info - Step 6: Got length buffer at address {}", len_ptr);
        
        let length_bytes = (output_size as u32).to_le_bytes();
        println!("[SGX] Getting module info - Step 7: Starting to write length bytes");
        let set_byte_name_c = CString::new("wbm_set_byte").unwrap();
        let mut arg_buf  = [wamr_sgx_val_t::default(); 2];
        arg_buf[1].value.i32_ = 0i32;
        
        for i in 0..4 {
            println!("[SGX] Getting module info - Step 7.{}: Writing byte {} of length", i+1, i+1);

            arg_buf[0].value.i32_ = length_bytes[i] as i32;
            arg_buf[1].value.i32_ = (len_ptr + i as u32) as i32;

            let mut ret_val = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
            };
            
            let set_byte_result = unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    set_byte_name_c.as_ptr(),
                    arg_buf.as_mut_ptr(),
                    2,
                    &mut ret_val,
                    1
                )
            };
            
            if set_byte_result != 0 {
                return Err(Error::FunctionCallFailed(set_byte_result));
            }
        }
        
        println!("[SGX] Getting module info - Step 8: Calling get_module_info WASM function");
        let mut info_arg1 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_ptr as i32 },
        };
        
        let mut info_arg2 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: len_ptr as i32 },
        };
        
        let mut info_return = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 0 },
        };
        
        let info_name_c = CString::new("get_module_info").unwrap();
        let mut info_args = [info_arg1, info_arg2];
        
        println!("[SGX] Getting module info - Step 8.1: About to make actual WASM call");
        let info_result = unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.module.instance,
                info_name_c.as_ptr(),
                info_args.as_mut_ptr(),
                2,
                &mut info_return,
                1
            )
        };
        println!("[SGX] Getting module info - Step 8.2: WASM call completed with result {}", info_result);
        
        let result_code = unsafe { info_return.value.i32_ };
        println!("[SGX] Getting module info - Step 9: Processing result code: {}", result_code);
        
        if info_result != 0 || result_code != 0 {
            let free_name_c = CString::new("wbm_free").unwrap();
            
            let mut free_output_arg1 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_ptr as i32 },
            };
            
            let mut free_output_arg2 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_size },
            };
            
            let mut free_args = [free_output_arg1, free_output_arg2];
            unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    free_name_c.as_ptr(),
                    free_args.as_mut_ptr(),
                    2,
                    std::ptr::null_mut(),
                    0
                )
            };
            
            let mut free_len_arg1 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: len_ptr as i32 },
            };
            
            let mut free_len_arg2 = wamr_sgx_val_t {
                type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 4 },
            };
            
            let mut free_args = [free_len_arg1, free_len_arg2];
            unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    free_name_c.as_ptr(),
                    free_args.as_mut_ptr(),
                    2,
                    std::ptr::null_mut(),
                    0
                )
            };
            
            if info_result != 0 {
                return Err(Error::FunctionCallFailed(info_result));
            } else {
                return Err(Error::FunctionCallFailed(result_code));
            }
        }
        
        let mut len_data = [0u8; 4];
        println!("[SGX] Getting module info - Step 10: Reading length bytes");

        let get_byte_name_c = CString::new("wbm_get_byte").unwrap();
        let mut arg_buf  = [wamr_sgx_val_t::default(); 2];
        arg_buf[1].value.i32_ = 0i32;
        
        for i in 0..4 {
            println!("[SGX] Getting module info - Step 10.{}: Reading byte {} of length", i+1, i+1);
            arg_buf[0].value.i32_ = (len_ptr + i as u32) as i32;

            let read_result = unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    get_byte_name_c.as_ptr(),
                    &mut arg_buf[0],
                    1,
                    &mut arg_buf[1],
                    1
                )
            };
            
            if read_result != 0 {
                let free_name_c = CString::new("wbm_free").unwrap();
                
                let mut free_output_arg1 = wamr_sgx_val_t {
                    type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                    value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_ptr as i32 },
                };
                
                let mut free_output_arg2 = wamr_sgx_val_t {
                    type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                    value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_size },
                };
                
                let mut free_args = [free_output_arg1, free_output_arg2];
                unsafe {
                    wamr_sgx_call_function(
                        SGX_CONTEXT,
                        self.module.instance,
                        free_name_c.as_ptr(),
                        free_args.as_mut_ptr(),
                        2,
                        std::ptr::null_mut(),
                        0
                    )
                };
                
                let mut free_len_arg1 = wamr_sgx_val_t {
                    type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                    value: wamr_sgx_val_t__bindgen_ty_1 { i32_: len_ptr as i32 },
                };
                
                let mut free_len_arg2 = wamr_sgx_val_t {
                    type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
                    value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 4 },
                };
                
                let mut free_args = [free_len_arg1, free_len_arg2];
                unsafe {
                    wamr_sgx_call_function(
                        SGX_CONTEXT,
                        self.module.instance,
                        free_name_c.as_ptr(),
                        free_args.as_mut_ptr(),
                        2,
                        std::ptr::null_mut(),
                        0
                    )
                };
                
                return Err(Error::FunctionCallFailed(read_result));
            }
            
            len_data[i] = unsafe { arg_buf[1].value.i32_ as u8 };
        }
        
        let output_len = u32::from_le_bytes(len_data);
        println!("[SGX] Getting module info - Step 11: Read length value: {} bytes", output_len);
        
        let mut output_data = Vec::with_capacity(output_len as usize);
        println!("[SGX] Getting module info - Step 12: Reading {} bytes of data", output_len);
        
        let max_bytes_to_read = std::cmp::min(output_len, 100);
        println!("[SGX] Getting module info - Step 12.1: Limited to first {} bytes for debugging", max_bytes_to_read);
        let get_byte_name_c = CString::new("wbm_get_byte").unwrap();
        let mut arg_buf  = [wamr_sgx_val_t::default(); 2];
        arg_buf[1].value.i32_ = 0i32;
        
        for i in 0..max_bytes_to_read {
            if i % 10 == 0 {
                println!("[SGX] Getting module info - Step 12.2: Reading byte {}/{}", i, max_bytes_to_read);
            }
            arg_buf[0].value.i32_ = (output_ptr + i) as i32;

            let read_result = unsafe {
                wamr_sgx_call_function(
                    SGX_CONTEXT,
                    self.module.instance,
                    get_byte_name_c.as_ptr(),
                    &mut arg_buf[0],
                    1,
                    &mut arg_buf[1],
                    1
                )
            };
            
            if read_result != 0 {
                break;
            }
            
            output_data.push(unsafe { arg_buf[1].value.i32_ as u8 });
        }
        
        println!("[SGX] Getting module info - Step 13: Freeing memory");
        let free_name_c = CString::new("wbm_free").unwrap();
        
        let mut free_output_arg1 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_ptr as i32 },
        };
        
        let mut free_output_arg2 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: output_size },
        };
        
        let mut free_args = [free_output_arg1, free_output_arg2];
        unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.module.instance,
                free_name_c.as_ptr(),
                free_args.as_mut_ptr(),
                2,
                std::ptr::null_mut(),
                0
            )
        };
        
        let mut free_len_arg1 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: len_ptr as i32 },
        };
        
        let mut free_len_arg2 = wamr_sgx_val_t {
            type_: wamr_sgx_val_type_t_WAMR_SGX_VAL_TYPE_I32,
            value: wamr_sgx_val_t__bindgen_ty_1 { i32_: 4 },
        };
        
        let mut free_args = [free_len_arg1, free_len_arg2];
        unsafe {
            wamr_sgx_call_function(
                SGX_CONTEXT,
                self.module.instance,
                free_name_c.as_ptr(),
                free_args.as_mut_ptr(),
                2,
                std::ptr::null_mut(),
                0
            )
        };
        
        println!("[SGX] Getting module info - Step 14: Converting output to string");
        let output_str = String::from_utf8(output_data)
            .map_err(|_| Error::InvalidArgument("Output is not valid UTF-8".to_string()))?;
        
        println!("[SGX] Getting module info - Step 15: Successfully retrieved module info");
        Ok(output_str)
    }
    
    pub fn estimate_gas(&self, tx_json: &str, state_json: &str) -> Result<u64, Error> {
        println!("[SGX] Starting gas estimation. TX size: {} bytes, State size: {} bytes", 
            tx_json.len(), state_json.len());
        
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
        
        println!("[SGX] Calling estimate_gas function");
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
        
        println!("[SGX] Freeing input memory");
        let free_tx_args = vec![Value::I32(tx_ptr as i32), Value::I32(tx_json.len() as i32)];
        let _ = self.module.call_function("wbm_free", &free_tx_args)?;
        
        let free_state_args = vec![Value::I32(state_ptr as i32), Value::I32(state_json.len() as i32)];
        let _ = self.module.call_function("wbm_free", &free_state_args)?;
        
        if result_code != 0 {
            let free_result_args = vec![Value::I32(result_ptr as i32), Value::I32(8)];
            let _ = self.module.call_function("wbm_free", &free_result_args)?;
            
            return Err(Error::FunctionCallFailed(result_code));
        }
        
        println!("[SGX] Reading gas estimate result");
        let result_data = self.module.read_memory(result_ptr, 8)?;
        let gas_estimate = u64::from_le_bytes([
            result_data[0], result_data[1], result_data[2], result_data[3],
            result_data[4], result_data[5], result_data[6], result_data[7]
        ]);
        
        let free_result_args = vec![Value::I32(result_ptr as i32), Value::I32(8)];
        let _ = self.module.call_function("wbm_free", &free_result_args)?;
        
        println!("[SGX] Gas estimation completed successfully: {}", gas_estimate);
        Ok(gas_estimate)
    }
}