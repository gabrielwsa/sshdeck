//! Clipboard do sistema para copiar senhas, limpo sozinho depois de CLEAR_AFTER.

use anyhow::{Result, anyhow};
use std::{thread, time::Duration};
use zeroize::Zeroizing;

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
        cb.set_text(text.to_owned()).map_err(|e| anyhow!("{e}"))?;
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
