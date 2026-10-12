//! Geração de ~/.ssh/sshdeck.conf e import do ~/.ssh/config existente.

use crate::vault::Host;
use anyhow::{Context, Result};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const MANAGED_FILE: &str = "sshdeck.conf";

fn ssh_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("HOME não encontrado")?
        .join(".ssh"))
}

/// Regrava o arquivo gerenciado e garante o `Include` no ~/.ssh/config.
pub fn write(hosts: &[Host]) -> Result<()> {
    let dir = ssh_dir()?;
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }

    let mut out = String::from("# Gerado pelo sshdeck — não edite à mão, use o sshdeck.\n");
    for h in hosts {
        out.push_str(&format!(
            "\nHost {}\n    HostName {}\n",
            h.alias, h.hostname
        ));
        if !h.user.is_empty() {
            out.push_str(&format!("    User {}\n", h.user));
        }
        out.push_str(&format!("    Port {}\n", h.port));
        if !h.identity_file.is_empty() {
            out.push_str(&format!("    IdentityFile {}\n", h.identity_file));
        }
    }
    let path = dir.join(MANAGED_FILE);
    fs::write(&path, out)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    ensure_include(&dir)
}

/// O Include precisa ficar no topo: depois de um `Host`, ele valeria só para aquele bloco.
fn ensure_include(dir: &Path) -> Result<()> {
    let config = dir.join("config");
    let current = fs::read_to_string(&config).unwrap_or_default();
    let already = current.lines().any(|l| {
        let l = l.trim().to_lowercase();
        l.starts_with("include") && l.contains(MANAGED_FILE)
    });
    if already {
        return Ok(());
    }
    if config.exists() {
        let backup = dir.join("config.sshdeck.bak");
        if !backup.exists() {
            fs::copy(&config, &backup)?;
        }
    }
    let new = format!("Include ~/.ssh/{MANAGED_FILE}\n\n{current}");
    fs::write(&config, new)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Lê os blocos `Host` do ~/.ssh/config (ignora curingas e o arquivo do sshdeck).
pub fn import_existing() -> Result<Vec<Host>> {
    let config = ssh_dir()?.join("config");
    let text = fs::read_to_string(&config).unwrap_or_default();
    let mut hosts = Vec::new();
    let mut current: Vec<Host> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (
                k.to_lowercase(),
                v.trim_start_matches(['=', ' ', '\t']).trim(),
            ),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                hosts.append(&mut current);
                current = value
                    .split_whitespace()
                    .filter(|a| !a.contains(['*', '?', '!']))
                    .map(|a| Host {
                        alias: a.to_string(),
                        port: 22,
                        ..Default::default()
                    })
                    .collect();
            }
            "match" => hosts.append(&mut current),
            "hostname" => current
                .iter_mut()
                .for_each(|h| h.hostname = value.to_string()),
            "user" => current.iter_mut().for_each(|h| h.user = value.to_string()),
            "port" => current
                .iter_mut()
                .for_each(|h| h.port = value.parse().unwrap_or(22)),
            "identityfile" => current
                .iter_mut()
                .for_each(|h| h.identity_file = value.to_string()),
            _ => {}
        }
    }
    hosts.append(&mut current);
    for h in &mut hosts {
        if h.hostname.is_empty() {
            h.hostname = h.alias.clone();
        }
    }
    Ok(hosts)
}

/// O que o ssh-copy-id usa quando o IdentityFile fica vazio, como texto de ajuda:
/// as chaves do ssh-agent, se houver; senão o ~/.ssh/id*.pub modificado mais recentemente.
pub fn default_key_hint() -> String {
    if std::env::var_os("SSH_AUTH_SOCK").is_some()
        && let Ok(out) = std::process::Command::new("ssh-add").arg("-L").output()
        && out.status.success()
    {
        let n = String::from_utf8_lossy(&out.stdout).lines().count();
        if n > 0 {
            return format!("vazio = {n} chave(s) do ssh-agent");
        }
    }
    let newest = ssh_dir().ok().and_then(|dir| {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("id") && n.ends_with(".pub") && !n.ends_with("-cert.pub")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
    });
    match newest {
        Some(e) => format!("vazio = ~/.ssh/{}", e.file_name().to_string_lossy()),
        None => "vazio = nenhuma chave em ~/.ssh (rode ssh-keygen)".into(),
    }
}
