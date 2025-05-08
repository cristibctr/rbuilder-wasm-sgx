use alloy_primitives::Bytes;
use ecdsa::signature::Signer;
use k256::ecdsa::{SigningKey, Signature, VerifyingKey, signature::Verifier};
use sha3::{Digest, Keccak256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Signing error: {0}")]
    SigningError(String),
    
    #[error("Verification error: {0}")]
    VerificationError(String),
    
    #[error("Key error: {0}")]
    KeyError(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

pub struct BlockSigner {
    private_key: SigningKey,
}

impl BlockSigner {
    pub fn new() -> Result<Self> {
        let private_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";
        
        let key_bytes = hex::decode(private_key_hex)
            .map_err(|e| CryptoError::KeyError(format!("Failed to decode key: {}", e)))?;
            
        let private_key = SigningKey::from_bytes(key_bytes.as_slice().into())
            .map_err(|e| CryptoError::KeyError(format!("Invalid private key: {}", e)))?;
        
        Ok(Self { private_key })
    }
    
    pub fn sign(&self, data: &[u8]) -> Result<Bytes> {
        let mut hasher = Keccak256::new();
        hasher.update(data);
        let hash = hasher.finalize();
        
        let signature: Signature = self.private_key.sign(&hash);
        
        Ok(Bytes::from(signature.to_bytes().to_vec()))
    }
    
    pub fn public_key(&self) -> VerifyingKey {
        *self.private_key.verifying_key()
    }
}

pub fn verify_signature(data: &[u8], signature: &[u8], public_key: &VerifyingKey) -> Result<bool> {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let hash = hasher.finalize();
    
    let sig = Signature::try_from(signature)
        .map_err(|e| CryptoError::VerificationError(format!("Invalid signature format: {}", e)))?;
    
    let is_valid = public_key.verify(&hash, &sig).is_ok();
    
    Ok(is_valid)
}
