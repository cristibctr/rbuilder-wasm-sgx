use alloy_primitives::Bytes;
use k256::ecdsa::{signature::Verifier, VerifyingKey, Signature};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use thiserror::Error;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBuilderOutput {
    pub header: serde_json::Value,
    pub transactions: serde_json::Value,
    pub receipts: serde_json::Value, 
    pub state_diff: serde_json::Value,
    pub state_root: serde_json::Value,
    pub metrics: serde_json::Value,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<serde_json::Value>,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_info: Option<serde_json::Value>,
    
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_requests: Option<serde_json::Value>,
    
    pub signature: Option<Bytes>,
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
        
        let mut json_value: serde_json::Value = serde_json::from_str(output_json)?;
        
        tracing::debug!("Extracting signature from JSON value");
        let signature = match json_value.get("signature") {
            Some(sig_value) => {
                let sig_str = sig_value.as_str().ok_or_else(|| {
                    SignatureVerificationError::InvalidSignatureFormat("Signature is not a string".to_string())
                })?;
                
                let sig_hex = if sig_str.starts_with("0x") {
                    &sig_str[2..]
                } else {
                    sig_str
                };
                
                let sig_bytes = hex::decode(sig_hex).map_err(|e| {
                    SignatureVerificationError::InvalidSignatureFormat(format!("Invalid hex in signature: {}", e))
                })?;
                
                tracing::debug!("Signature found: 0x{}", hex::encode(&sig_bytes));
                Bytes::from(sig_bytes)
            },
            None => {
                tracing::warn!("No signature field found in output JSON");
                return Err(SignatureVerificationError::MissingSignature);
            }
        };
        
        if let Some(obj) = json_value.as_object_mut() {
            obj.remove("signature");
        }
        
        tracing::debug!("Re-serializing data with signature removed (sorted)");
        let data_to_verify = serde_json::to_string(&json_value)?;
        tracing::debug!("Serialized data size: {} bytes", data_to_verify.len());
        tracing::debug!("Serialized data: {}", &data_to_verify.chars().collect::<String>());
        
        tracing::debug!("Hashing data with Keccak256");
        let mut hasher = Keccak256::new();
        hasher.update(data_to_verify.as_bytes());
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
        
        let mut json_value: serde_json::Value = serde_json::from_str(output_json)?;
        
        tracing::debug!("Extracting signature from JSON value");
        let signature = match json_value.get("signature") {
            Some(sig_value) => {
                let sig_str = sig_value.as_str().ok_or_else(|| {
                    SignatureVerificationError::InvalidSignatureFormat("Signature is not a string".to_string())
                })?;
                
                let sig_hex = if sig_str.starts_with("0x") {
                    &sig_str[2..]
                } else {
                    sig_str
                };
                
                let sig_bytes = hex::decode(sig_hex).map_err(|e| {
                    SignatureVerificationError::InvalidSignatureFormat(format!("Invalid hex in signature: {}", e))
                })?;
                
                tracing::debug!("Signature found: 0x{}", hex::encode(&sig_bytes));
                Bytes::from(sig_bytes)
            },
            None => {
                tracing::warn!("No signature field found in output JSON");
                return Err(SignatureVerificationError::MissingSignature);
            }
        };
        
        if let Some(obj) = json_value.as_object_mut() {
            obj.remove("signature");
        }
        
        tracing::debug!("Re-serializing data with signature removed");
        let data_to_verify = json_value.to_string();
        
        tracing::debug!("Hashing data with Keccak256");
        let mut hasher = Keccak256::new();
        hasher.update(data_to_verify.as_bytes());
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