//! RDP: o app desenha a sessão com o IronRDP (gui/src-tauri/src/rdpview.rs). Aqui fica o que não
//! depende da janela: validação do cadastro e o túnel `ssh -L` para Windows atrás de um servidor Linux.

use crate::{askpass, vault::Host};
use anyhow::Result;
use std::{
    net::{Ipv4Addr, TcpListener},
    process::{Command, Stdio},
};

pub const DEFAULT_PORT: u16 = 3389;

/// Valida o campo de resolução: vazio (acompanha o tamanho da aba) ou "LxA" fixo.
pub fn valid_resolution(r: &str) -> bool {
    let r = r.trim();
    if r.is_empty() {
        return true;
    }
    match r.split_once(['x', 'X']) {
        Some((w, h)) => {
            let ok = |v: &str| v.parse::<u16>().is_ok_and(|n| (200..=8192).contains(&n));
            ok(w) && ok(h)
        }
        None => false,
    }
}

/// `ssh -N -L 127.0.0.1:port:host:host_port gw`: o túnel até o RDP passando por um host SSH.
/// A senha salva do host de passagem é respondida pelo askpass (sem terminal).
pub fn tunnel_command(gw: &Host, port: u16, host: &str, host_port: u16) -> Result<Command> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-N")
        .arg("-L")
        .arg(format!("127.0.0.1:{port}:{host}:{host_port}"))
        .args([
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=30",
            // conexão própria: com ControlMaster no ~/.ssh/config, o forward iria para o
            // master e este processo sairia na hora (parecendo que o túnel caiu)
            "-o",
            "ControlPath=none",
        ])
        .arg(&gw.alias)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", std::env::current_exe()?)
        .env(askpass::NO_TTY_ENV, "1");
    if !gw.password.is_empty() {
        cmd.env(askpass::PASSWORD_ENV, &gw.password);
    }
    Ok(cmd)
}

/// Uma porta livre em 127.0.0.1 (para a ponta local do túnel).
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

#[cfg(test)]
mod tests {
    use super::valid_resolution;

    #[test]
    fn resolution() {
        assert!(valid_resolution(""));
        assert!(valid_resolution("1920x1080"));
        assert!(valid_resolution(" 1280X800 "));
        assert!(!valid_resolution("full"));
        assert!(!valid_resolution("90%"));
        assert!(!valid_resolution("10x10"));
        assert!(!valid_resolution("abc"));
    }
}
