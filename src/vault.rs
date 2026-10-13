//! Cofre criptografado: guarda hosts + senhas num único arquivo.
//! Formato: MAGIC(6) | salt(16) | nonce(24) | ciphertext (JSON do Store).
//! Chave derivada da senha mestra com Argon2id; cifra XChaCha20-Poly1305.

use anyhow::{Context, Result, anyhow, bail};
use argon2::Argon2;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, aead::Aead};
use rand::RngCore;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, os::unix::fs::PermissionsExt, path::PathBuf};
use zeroize::Zeroizing;

const MAGIC: &[u8; 6] = b"SSHDK1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum Kind {
    #[default]
    Ssh,
    Rdp,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Host {
    #[serde(default)]
    pub kind: Kind,
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub identity_file: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub notes: String,
    // --- só RDP ---
    #[serde(default)]
    pub domain: String,
    /// vazio = janela redimensionável; "full", "1920x1080" ou "90%"
    #[serde(default)]
    pub resolution: String,
    /// pasta local compartilhada com o Windows
    #[serde(default)]
    pub share: String,
    /// alias de um host SSH para abrir túnel até o RDP
    #[serde(default)]
    pub gateway: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Store {
    pub hosts: Vec<Host>,
    /// idioma escolhido com a tecla L
    #[serde(default)]
    pub lang: Option<String>,
}

pub struct Vault {
    path: PathBuf,
    salt: [u8; SALT_LEN],
    key: Zeroizing<[u8; 32]>,
}

impl Vault {
    pub fn default_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
            .join("sshdeck")
            .join("vault.bin")
    }

    pub fn create(path: PathBuf, password: &str) -> Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        let key = derive_key(password, &salt)?;
        let vault = Self { path, salt, key };
        vault.save(&Store::default())?;
        Ok(vault)
    }

    pub fn open(path: PathBuf, password: &str) -> Result<(Self, Store)> {
        let data = fs::read(&path).with_context(|| t!("vault.reading", path = path.display()))?;
        let header = MAGIC.len() + SALT_LEN + NONCE_LEN;
        if data.len() < header || &data[..MAGIC.len()] != MAGIC {
            bail!("{}", t!("vault.invalid"));
        }
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&data[MAGIC.len()..MAGIC.len() + SALT_LEN]);
        let nonce = &data[MAGIC.len() + SALT_LEN..header];
        let key = derive_key(password, &salt)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|e| anyhow!("{e}"))?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    nonce.try_into().map_err(|_| anyhow!("invalid nonce"))?,
                    &data[header..],
                )
                .map_err(|_| anyhow!("{}", t!("vault.wrong_password")))?,
        );
        let store: Store = serde_json::from_slice(&plain).context(t!("vault.corrupt"))?;
        Ok((Self { path, salt, key }, store))
    }

    /// Troca a senha mestra (novo salt) e regrava o cofre.
    pub fn rekey(&mut self, password: &str, store: &Store) -> Result<()> {
        rand::thread_rng().fill_bytes(&mut self.salt);
        self.key = derive_key(password, &self.salt)?;
        self.save(store)
    }

    pub fn save(&self, store: &Store) -> Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(store)?);
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
        let cipher =
            XChaCha20Poly1305::new_from_slice(self.key.as_ref()).map_err(|e| anyhow!("{e}"))?;
        let ct = cipher
            .encrypt(
                (&nonce[..])
                    .try_into()
                    .map_err(|_| anyhow!("invalid nonce"))?,
                plain.as_slice(),
            )
            .map_err(|_| anyhow!("{}", t!("vault.encrypt_failed")))?;

        let dir = self.path.parent().context(t!("vault.no_dir"))?;
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;

        // grava num temporário e renomeia, para nunca deixar o cofre pela metade
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
            f.write_all(MAGIC)?;
            f.write_all(&self.salt)?;
            f.write_all(&nonce)?;
            f.write_all(&ct)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_password() {
        let dir = std::env::temp_dir().join(format!("sshdeck-test-{}", std::process::id()));
        let path = dir.join("vault.bin");
        let v = Vault::create(path.clone(), "mestra").unwrap();
        let store = Store {
            hosts: vec![Host {
                alias: "prod".into(),
                hostname: "10.0.0.1".into(),
                port: 22,
                password: "s3nh@ x".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        v.save(&store).unwrap();

        let raw = fs::read(&path).unwrap();
        assert!(
            !raw.windows(5).any(|w| w == b"s3nh@"),
            "senha em texto puro no arquivo"
        );

        let (_, loaded) = Vault::open(path.clone(), "mestra").unwrap();
        assert_eq!(loaded.hosts[0].password, "s3nh@ x");
        assert!(Vault::open(path, "errada").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
