use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    ChaCha20Poly1305, Key,
};
use openterm_core::{EncryptedSecret, KdfParams, SecretId};
use zeroize::Zeroize;

const SECRET_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

#[derive(Debug, Clone)]
pub struct VaultConfig {
    pub memory_cost_kib: u32,
    pub time_cost: u32,
    pub parallelism: u32,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            memory_cost_kib: 19 * 1024,
            time_cost: 2,
            parallelism: 1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("invalid vault parameters")]
    InvalidParams,
    #[error("key derivation failed")]
    KeyDerivation,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
}

pub struct LocalVault {
    config: VaultConfig,
}

impl LocalVault {
    pub fn new(config: VaultConfig) -> Self {
        Self { config }
    }

    pub fn encrypt_secret(
        &self,
        master_password: &[u8],
        plaintext: &[u8],
    ) -> Result<EncryptedSecret, VaultError> {
        let salt = random_bytes::<SALT_LEN>();
        let mut key = self.derive_key(master_password, &salt)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| VaultError::Encrypt)?;
        key.zeroize();

        Ok(EncryptedSecret {
            id: SecretId::new(),
            version: SECRET_VERSION,
            salt: salt.to_vec(),
            nonce: nonce.to_vec(),
            ciphertext,
            kdf: KdfParams {
                algorithm: "argon2id".to_string(),
                memory_cost_kib: self.config.memory_cost_kib,
                time_cost: self.config.time_cost,
                parallelism: self.config.parallelism,
            },
        })
    }

    pub fn decrypt_secret(
        &self,
        master_password: &[u8],
        secret: &EncryptedSecret,
    ) -> Result<Vec<u8>, VaultError> {
        let mut key = derive_key_with_params(master_password, &secret.salt, &secret.kdf)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let plaintext = cipher
            .decrypt(secret.nonce.as_slice().into(), secret.ciphertext.as_ref())
            .map_err(|_| VaultError::Decrypt)?;
        key.zeroize();
        Ok(plaintext)
    }

    fn derive_key(&self, master_password: &[u8], salt: &[u8]) -> Result<[u8; KEY_LEN], VaultError> {
        let params = KdfParams {
            algorithm: "argon2id".to_string(),
            memory_cost_kib: self.config.memory_cost_kib,
            time_cost: self.config.time_cost,
            parallelism: self.config.parallelism,
        };
        derive_key_with_params(master_password, salt, &params)
    }
}

fn derive_key_with_params(
    master_password: &[u8],
    salt: &[u8],
    params: &KdfParams,
) -> Result<[u8; KEY_LEN], VaultError> {
    if params.algorithm != "argon2id" {
        return Err(VaultError::InvalidParams);
    }

    let params = Params::new(
        params.memory_cost_kib,
        params.time_cost,
        params.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|_| VaultError::InvalidParams)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0_u8; KEY_LEN];
    argon2
        .hash_password_into(master_password, salt, &mut key)
        .map_err(|_| VaultError::KeyDerivation)?;
    Ok(key)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use chacha20poly1305::aead::rand_core::RngCore;

    let mut bytes = [0_u8; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_decrypts_secret() {
        let vault = LocalVault::new(VaultConfig {
            memory_cost_kib: 256,
            time_cost: 1,
            parallelism: 1,
        });

        let secret = vault.encrypt_secret(b"master", b"ssh-password").unwrap();
        assert_ne!(secret.ciphertext, b"ssh-password");

        let plaintext = vault.decrypt_secret(b"master", &secret).unwrap();
        assert_eq!(plaintext, b"ssh-password");
    }

    #[test]
    fn wrong_password_fails() {
        let vault = LocalVault::new(VaultConfig {
            memory_cost_kib: 256,
            time_cost: 1,
            parallelism: 1,
        });
        let secret = vault.encrypt_secret(b"master", b"ssh-password").unwrap();

        assert!(vault.decrypt_secret(b"wrong", &secret).is_err());
    }
}
