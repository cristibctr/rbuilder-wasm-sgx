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
    #[serde(flatten)]
    pub data: serde_json::Value,
    
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
        let mut output: BlockBuilderOutput = serde_json::from_str(output_json)?;
        let signature = match output.signature.take() {
            Some(sig) => sig,
            None => return Err(SignatureVerificationError::MissingSignature),
        };
        output.signature = None;
        let data_to_verify = serde_json::to_vec(&output)?;
        let mut hasher = Keccak256::new();
        hasher.update(&data_to_verify);
        let hash = hasher.finalize();
        let sig = Signature::try_from(signature.as_ref())
            .map_err(|e| SignatureVerificationError::InvalidSignatureFormat(format!("Invalid signature format: {}", e)))?;
        let is_valid = self.public_key.verify(&hash, &sig).is_ok();
        
        if !is_valid {
            tracing::warn!("Signature verification failed for block output");
            return Err(SignatureVerificationError::VerificationFailed);
        }
        
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
        let signature = match output.signature.take() {
            Some(sig) => sig,
            None => return Err(SignatureVerificationError::MissingSignature),
        };
        output.signature = None;
        let data_to_verify = serde_json::to_vec(&output)?;
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