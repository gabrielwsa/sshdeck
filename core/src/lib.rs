//! Núcleo do sshdeck, sem interface: cofre, ssh config, rsync, túnel do RDP, askpass e idioma.

pub mod askpass;
pub mod complete;
pub mod hosts;
pub mod lang;
pub mod rdp;
pub mod sshconfig;
pub mod sysinfo;
pub mod transfer;
pub mod vault;

// traduções em locales/app.yml; o idioma escolhido vale também para o app (gui/src-tauri)
rust_i18n::i18n!("../locales", fallback = "en");
