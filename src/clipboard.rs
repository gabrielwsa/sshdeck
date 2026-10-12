//! Clipboard do sistema (Wayland/X11 via arboard) com fallback para OSC 52.

use base64::Engine;
use std::{io::Write, thread, time::Duration};
use zeroize::Zeroizing;

pub const CLEAR_AFTER: Duration = Duration::from_secs(45);

pub struct Clip {
    // precisa viver enquanto o app roda: no X11 o conteúdo some quando o dono é destruído
    inner: Option<arboard::Clipboard>,
}

impl Clip {
    pub fn new() -> Self {
        Self {
            inner: arboard::Clipboard::new().ok(),
        }
    }

    /// Copia o texto e agenda a limpeza. Retorna o método usado.
    pub fn copy_secret(&mut self, text: &str) -> &'static str {
        if let Some(cb) = &mut self.inner
            && cb.set_text(text.to_owned()).is_ok()
        {
            schedule_clear(Zeroizing::new(text.to_owned()));
            return "clipboard";
        }
        osc52(text);
        "OSC 52"
    }
}

/// Limpa o clipboard depois de CLEAR_AFTER, se ele ainda contiver a senha.
/// Roda numa thread para funcionar mesmo enquanto o ssh está em primeiro plano.
fn schedule_clear(secret: Zeroizing<String>) {
    thread::spawn(move || {
        thread::sleep(CLEAR_AFTER);
        if let Ok(mut cb) = arboard::Clipboard::new()
            && cb.get_text().ok().as_deref() == Some(secret.as_str())
        {
            let _ = cb.clear();
        }
    });
}

/// Sequência OSC 52: o próprio terminal copia (funciona em kitty, foot, wezterm, alacritty, tmux...).
fn osc52(text: &str) {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();
}
