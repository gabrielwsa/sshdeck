//! Clipboard do sistema para copiar senhas, limpo sozinho depois de CLEAR_AFTER.

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

/// Clipboard do sistema usado pelo RDP: um só para o processo, porque no X11 o texto copiado
/// some quando o dono é destruído.
static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Shared {
    clipboard: Option<arboard::Clipboard>,
    /// Não devolve a outro servidor (nem ao próprio) texto que veio do RDP.
    remote: Option<[u8; 32]>,
}

impl Shared {
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    fn local_text(&mut self, text: String) -> Option<String> {
        let digest = hash(&text);
        if self.remote == Some(digest) {
            return None;
        }
        self.remote = None;
        (!text.is_empty() && !is_secret(&text)).then_some(text)
    }
}

/// Hash das senhas copiadas pelo app (só o hash): o RDP não as anuncia ao Windows.
static SECRETS: LazyLock<Mutex<HashSet<[u8; 32]>>> = LazyLock::new(Mutex::default);

pub fn hash(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

pub fn is_secret(text: &str) -> bool {
    SECRETS.lock().unwrap().contains(&hash(text))
}

pub(crate) fn publish_secret<T>(text: &str, publish: impl FnOnce() -> T) -> T {
    SECRETS.lock().unwrap().insert(hash(text));
    publish()
}

/// Texto local que pode ir para um servidor RDP (não inclui senhas nem texto de outro RDP).
pub fn local_text() -> Option<String> {
    let mut shared = SHARED.lock().unwrap();
    let text = shared.clipboard()?.get_text().ok()?;
    shared.local_text(text)
}

pub fn set_remote_text(text: &str) -> Option<()> {
    let mut shared = SHARED.lock().unwrap();
    shared.clipboard()?.set_text(text.to_owned()).ok()?;
    shared.remote = Some(hash(text));
    Some(())
}

pub const CLEAR_AFTER: Duration = Duration::from_secs(45);

#[derive(Default)]
pub struct Clip {
    // precisa viver enquanto o app roda: no X11 o conteúdo some quando o dono é destruído
    inner: Option<arboard::Clipboard>,
}

impl Clip {
    pub fn copy_secret(&mut self, text: &str) -> Result<&'static str> {
        if self.inner.is_none() {
            self.inner = Some(arboard::Clipboard::new().map_err(|e| anyhow!("{e}"))?);
        }
        let cb = self.inner.as_mut().expect("acabou de ser criado");
        publish_secret(text, || cb.set_text(text.to_owned())).map_err(|e| anyhow!("{e}"))?;
        let secret = Zeroizing::new(text.to_owned());
        thread::spawn(move || {
            thread::sleep(CLEAR_AFTER);
            // só limpa se ainda for a senha: não apaga algo que o usuário copiou depois
            if let Ok(mut cb) = arboard::Clipboard::new()
                && cb.get_text().ok().as_deref() == Some(secret.as_str())
            {
                let _ = cb.clear();
            }
        });
        Ok("clipboard")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denylist_is_updated_before_publishing_the_secret() {
        let secret = "senha-de-teste-antes-de-publicar";
        publish_secret(secret, || assert!(is_secret(secret)));
    }

    #[test]
    fn remote_text_is_not_local_after_switching_servers() {
        let mut shared = Shared {
            remote: Some(hash("texto remoto")),
            ..Default::default()
        };
        assert!(shared.local_text("texto remoto".into()).is_none());
        assert!(shared.local_text("texto remoto".into()).is_none());
        assert_eq!(
            shared.local_text("texto local".into()).as_deref(),
            Some("texto local")
        );
        assert_eq!(
            shared.local_text("texto remoto".into()).as_deref(),
            Some("texto remoto")
        );
    }
}
