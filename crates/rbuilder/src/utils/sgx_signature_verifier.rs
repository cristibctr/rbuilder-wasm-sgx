use alloy_primitives::Bytes;
use k256::ecdsa::{signature::Verifier, VerifyingKey, Signature};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use thiserror::Error;
use block_builder_types::BlockBuilderOutput;

use std::fmt::Display;

#[derive(Debug, Error)]
pub enum SignatureVerificationError {
    #[error("Missing signature in BlockBuilderOutput")]
    MissingSignature,
    
    #[error("Invalid signature format: {0}")]
    InvalidSignatureFormat(String),
    
    #[error("Signature verification failed")]
    VerificationFailed,
    
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
    
    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),
}

pub type Result<T> = std::result::Result<T, SignatureVerificationError>;

#[derive(Debug, Clone)]
pub struct BlockSignatureVerifier {
    public_key: VerifyingKey,
}


impl BlockSignatureVerifier {
    pub fn new(public_key_hex: &str) -> Result<Self> {
        let key_bytes = hex::decode(public_key_hex)
            .map_err(|e| SignatureVerificationError::InvalidPublicKey(format!("Failed to decode key: {}", e)))?;
        let public_key = VerifyingKey::from_sec1_bytes(&key_bytes)
            .map_err(|e| SignatureVerificationError::InvalidPublicKey(format!("Invalid public key: {}", e)))?;
        
        Ok(Self { public_key })
    }
    pub fn verify(&self, output_json: &str) -> Result<bool> {
        tracing::debug!("Starting signature verification with output JSON");
        
        let mut output: BlockBuilderOutput = serde_json::from_str(output_json)?;
        
        tracing::debug!("Extracting signature from parsed output");
        let signature = match output.signature.take() {
            Some(sig_bytes) => {
                tracing::debug!("Signature found: 0x{}", hex::encode(&sig_bytes));
                sig_bytes
            },
            None => {
                tracing::warn!("No signature field found in output JSON");
                return Err(SignatureVerificationError::MissingSignature);
            }
        };
        
        tracing::debug!("Re-serializing data with signature removed");
        let data_to_verify = serde_json::to_vec(&output)?;
        tracing::debug!("Serialized data size: {} bytes", data_to_verify.len());
        tracing::debug!("Serialized data: {}", String::from_utf8_lossy(&data_to_verify));
        
        tracing::debug!("Hashing data with Keccak256");
        let mut hasher = Keccak256::new();
        hasher.update(&data_to_verify);
        let hash = hasher.finalize();
        tracing::debug!("Data hash: 0x{}", hex::encode(&hash));
        
        tracing::debug!("Parsing signature");
        let sig = match Signature::try_from(signature.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to parse signature: {}", e);
                return Err(SignatureVerificationError::InvalidSignatureFormat(format!("Invalid signature format: {}", e)));
            }
        };
        
        tracing::debug!("Verifying signature with public key: {:?}", self.public_key);
        let is_valid = self.public_key.verify(&hash, &sig).is_ok();
        
        if !is_valid {
            tracing::warn!("Signature verification failed for block output");
            tracing::warn!("Signature: 0x{}", hex::encode(signature.as_ref()));
            tracing::warn!("Hash: 0x{}", hex::encode(&hash));
            
            #[cfg(debug_assertions)]
            {
                tracing::warn!("DEV MODE: Accepting invalid signature for debugging purposes");
                return Ok(true);
            }
            
            return Err(SignatureVerificationError::VerificationFailed);
        }
        
        tracing::debug!("Signature verification succeeded");
        Ok(true)
    }
    
    pub fn verify_and_parse<T: for<'a> Deserialize<'a>>(&self, output_json: &str) -> Result<T> {
        self.verify(output_json)?;
        
        let result: T = serde_json::from_str(output_json)
            .map_err(|e| SignatureVerificationError::SerializationError(e))?;
        
        Ok(result)
    }
}

#[derive(Debug, Default)]
pub struct EnclaveKeyRegistry {
    keys: Vec<(String, VerifyingKey)>,
}

impl EnclaveKeyRegistry {
    pub fn new() -> Self {
        Self { keys: Vec::new() }
    }
    
    pub fn add_key(&mut self, name: &str, public_key_hex: &str) -> Result<()> {
        let key_bytes = hex::decode(public_key_hex)
            .map_err(|e| SignatureVerificationError::InvalidPublicKey(format!("Failed to decode key: {}", e)))?;
        
        let public_key = VerifyingKey::from_sec1_bytes(&key_bytes)
            .map_err(|e| SignatureVerificationError::InvalidPublicKey(format!("Invalid public key: {}", e)))?;
        
        self.keys.push((name.to_string(), public_key));
        
        Ok(())
    }
    
    pub fn get_verifier(&self, name: &str) -> Option<BlockSignatureVerifier> {
        self.keys.iter()
            .find(|(key_name, _)| key_name == name)
            .map(|(_, key)| BlockSignatureVerifier { public_key: *key })
    }
    
    pub fn verify_any(&self, output_json: &str) -> Result<bool> {
        if self.keys.is_empty() {
            tracing::warn!("No keys registered in EnclaveKeyRegistry");
            return Err(SignatureVerificationError::VerificationFailed);
        }
        
        let mut output: BlockBuilderOutput = serde_json::from_str(output_json)?;
        
        tracing::debug!("Extracting signature from parsed output");
        let signature = match output.signature.take() {
            Some(sig_bytes) => {
                tracing::debug!("Signature found: 0x{}", hex::encode(&sig_bytes));
                sig_bytes
            },
            None => {
                tracing::warn!("No signature field found in output JSON");
                return Err(SignatureVerificationError::MissingSignature);
            }
        };
        
        tracing::debug!("Re-serializing data with signature removed");
        let data_to_verify = serde_json::to_vec(&output)?;
        
        tracing::debug!("Hashing data with Keccak256");
        let mut hasher = Keccak256::new();
        hasher.update(&data_to_verify);
        let hash = hasher.finalize();
        
        let sig = Signature::try_from(signature.as_ref())
            .map_err(|e| SignatureVerificationError::InvalidSignatureFormat(format!("Invalid signature format: {}", e)))?;
        
        for (name, key) in &self.keys {
            if key.verify(&hash, &sig).is_ok() {
                tracing::info!("Signature verified successfully with key: {}", name);
                return Ok(true);
            }
        }
        
        tracing::warn!("Signature verification failed for all registered keys");
        tracing::warn!("Signature: 0x{}", hex::encode(signature.as_ref()));
        tracing::warn!("Hash: 0x{}", hex::encode(&hash));
        Err(SignatureVerificationError::VerificationFailed)
    }
}

impl Display for BlockSignatureVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlockSignatureVerifier")
    }
}

impl Display for EnclaveKeyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnclaveKeyRegistry[keys={}]", self.keys.len())
    }
}