use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit, OsRng},
};
use anyhow::{Result, anyhow};
use rand::RngCore;
use std::path::PathBuf;

const KEY_FILE: &str = "cuartel.key";

pub struct Vault {
    cipher: Aes256Gcm,
}

impl Vault {
    pub fn new(key: &[u8; 32]) -> Self {
        let key = Key::<Aes256Gcm>::from_slice(key);
        Self {
            cipher: Aes256Gcm::new(key),
        }
    }

    pub fn load_or_create() -> Result<Self> {
        let key_path = key_file_path();
        if key_path.exists() {
            let key_bytes = std::fs::read(&key_path)?;
            let key: [u8; 32] = key_bytes
                .try_into()
                .map_err(|_| anyhow!("invalid key file"))?;
            Ok(Self::new(&key))
        } else {
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            std::fs::create_dir_all(key_path.parent().unwrap())?;
            std::fs::write(&key_path, &key)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(Self::new(&key))
        }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("encryption failed: {}", e))?;
        Ok((ciphertext, nonce_bytes.to_vec()))
    }

    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
        let nonce = Nonce::from_slice(nonce);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| anyhow!("decryption failed: {}", e))
    }
}

fn key_file_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cuartel")
        .join(KEY_FILE)
}
