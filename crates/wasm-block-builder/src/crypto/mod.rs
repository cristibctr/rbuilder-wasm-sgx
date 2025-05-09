use alloy_primitives::Bytes;
use ecdsa::signature::Signer;
use k256::ecdsa::{SigningKey, Signature, VerifyingKey, signature::Verifier};
use sha3::{Digest, Keccak256};
use thiserror::Error;
use std::sync::{Mutex, OnceLock};
use rand_core::OsRng;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("Signing error: {0}")]
    SigningError(String),
    
    #[error("Verification error: {0}")]
    VerificationError(String),
    
    #[error("Key error: {0}")]
    KeyError(String),

    #[error("Key not initialized")]
    KeyNotInitialized,

    #[error("Key management error: {0}")]
    KeyManagementError(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

struct KeyStore {
    private_key: Option<SigningKey>,
}

static KEY_STORE: OnceLock<Mutex<KeyStore>> = OnceLock::new();

fn get_key_store() -> &'static Mutex<KeyStore> {
    KEY_STORE.get_or_init(|| {
        Mutex::new(KeyStore {
            private_key: None,
        })
    })
}

pub struct BlockSigner {
    private_key: SigningKey,
}

impl BlockSigner {
    pub fn new() -> Result<Self> {
        let private_key = Self::get_or_create_key()?;
        Ok(Self { private_key })
    }

    fn get_or_create_key() -> Result<SigningKey> {
        let mut key_store = get_key_store().lock()
            .map_err(|_| CryptoError::KeyManagementError("Failed to acquire key store lock".to_string()))?;

        if let Some(ref key) = key_store.private_key {
            return Ok(key.clone());
        }

        log::info!("Generating new ECDSA key pair for block signing");
        let private_key = Self::generate_new_key()?;

        key_store.private_key = Some(private_key.clone());

        // TODO: Implement key sealing for persistent storage

        Ok(private_key)
    }

    pub(crate) fn generate_new_key() -> Result<SigningKey> {
        #[cfg(feature = "test-key")]
        {
            log::warn!("Using test key - NOT SECURE FOR PRODUCTION");
            let private_key_hex = "0000000000000000000000000000000000000000000000000000000000000001";
            let key_bytes = hex::decode(private_key_hex)
                .map_err(|e| CryptoError::KeyError(format!("Failed to decode test key: {}", e)))?;
            SigningKey::from_bytes(key_bytes.as_slice().into())
                .map_err(|e| CryptoError::KeyError(format!("Invalid test key: {}", e)))
        }
        
        #[cfg(not(feature = "test-key"))]
        {
            Ok(SigningKey::random(&mut OsRng))
        }
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

pub fn get_current_public_key() -> Result<VerifyingKey> {
    let mut key_store = get_key_store().lock()
        .map_err(|_| CryptoError::KeyManagementError("Failed to acquire key store lock".to_string()))?;

    if let Some(ref private_key) = key_store.private_key {
        Ok(*private_key.verifying_key())
    } else {
        log::info!("Key not initialized. Generating new ECDSA key pair for the enclave");
        let private_key = BlockSigner::generate_new_key()?;
        key_store.private_key = Some(private_key.clone());

        Ok(*private_key.verifying_key())
    }
}

pub fn export_public_key_hex() -> Result<String> {
    let public_key = get_current_public_key()?;
    let encoded_key = public_key.to_encoded_point(true);
    let hex_key = hex::encode(encoded_key.as_bytes());
    Ok(hex_key)
}
