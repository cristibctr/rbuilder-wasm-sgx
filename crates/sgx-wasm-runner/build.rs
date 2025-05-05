use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    
    let sgx_sdk = env::var("SGX_SDK").unwrap_or_else(|_| "/opt/intel/sgxsdk".to_string());
    
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let wamr_path = PathBuf::from(manifest_dir).join("../wasm-micro-runtime")
        .canonicalize()
        .expect("failed to canonicalize WAMR path");
    
    println!("cargo:warning=Using SGX SDK: {}", sgx_sdk);
    println!("cargo:warning=Using WAMR path: {}", wamr_path.display());
    
    let sgx_dir = wamr_path.join("product-mini/platforms/linux-sgx");
    let build_dir = sgx_dir.join("build");
    let enclave_sample = sgx_dir.join("enclave-sample");
    
    std::fs::create_dir_all(&build_dir).unwrap_or_else(|e| {
        panic!("Failed to create build directory: {}", e);
    });
    
    let cmake_status = Command::new("cmake")
        .current_dir(&build_dir)
        .arg(format!("-DSGX_SDK={}", sgx_sdk))
        .arg("-DWAMR_BUILD_LIBRARY=ON")
        .arg("-DWAMR_BUILD_SIMD=0")
        .arg("..")
        .status()
        .expect("Failed to execute cmake");
        
    if !cmake_status.success() {
        panic!("CMake configuration failed");
    }

    let make_status = Command::new("make")
        .current_dir(&build_dir)
        .status()
        .expect("Failed to execute make");

    if !make_status.success() {
        panic!("Make build failed");
    }
    
    let make_status = Command::new("make")
        .current_dir(&enclave_sample)
        .arg("BUILD_LIB=1")
        .arg("CFLAGS='-O0 -g -fno-omit-frame-pointer'")
        .status()
        .expect("Failed to execute make");

    if !make_status.success() {
        panic!("Make build failed");
    }

    let bindings = bindgen::Builder::default()
        .header(sgx_dir.join("wamr_sgx_lib.h").to_str().unwrap())
        .clang_arg(format!("-I{}/include", sgx_sdk))
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write bindings");

    println!("cargo:rustc-link-search=native={}/lib64", sgx_sdk);
    
    println!("cargo:rustc-link-search=native={}", enclave_sample.display());
    println!("cargo:rustc-link-lib=static=wamr_sgx");

    if env::var("SGX_MODE").unwrap_or_else(|_| "SIM".to_string()) == "SIM" {
        println!("cargo:rustc-link-lib=dylib=sgx_uae_service_sim");
        println!("cargo:rustc-link-lib=dylib=sgx_urts_sim");
    } else {
        println!("cargo:rustc-link-lib=dylib=sgx_urts");
        println!("cargo:rustc-link-lib=dylib=sgx_uae_service");
    }

    println!("cargo:rustc-link-lib=dylib=pthread");

    println!("cargo:rerun-if-changed={}", sgx_dir.join("wamr_sgx_lib.h").display());
    println!("cargo:rerun-if-changed={}", sgx_dir.join("wamr_sgx_lib.c").display());
    println!("cargo:rerun-if-changed={}", enclave_sample.join("Enclave")
        .join("Enclave_library.cpp").display());
}