//! Informações do servidor (sistema, kernel, CPU, memória), coletadas por uma segunda conexão
//! ssh rápida em segundo plano, com a permissão do usuário. Ficam guardadas no cofre.

use crate::{askpass, vault};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Espera depois de abrir a sessão: a chave do servidor já foi aceita e a senha já foi testada.
pub const DELAY: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(20);
const SEPARATOR: &str = "@@sshdeck@@";

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SysInfo {
    pub os: String,
    pub hostname: String,
    pub kernel: String,
    pub arch: String,
    pub cpus: Option<u32>,
    pub mem_kb: Option<u64>,
    pub collected: u64,
}

/// Roda o comando no servidor e devolve as informações. Bloqueia: chamar numa thread.
pub fn collect(alias: &str, password: &str) -> Result<SysInfo> {
    let script = format!(
        "cat /etc/os-release 2>/dev/null; echo {SEPARATOR}; uname -n; uname -r; uname -m; \
         nproc 2>/dev/null; awk '/^MemTotal/ {{print $2}}' /proc/meminfo 2>/dev/null"
    );
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-T",
        "-o",
        "ConnectTimeout=10",
        // nunca pergunta sobre chave nova do servidor: em segundo plano não há quem responda
        "-o",
        "StrictHostKeyChecking=yes",
    ])
    .arg(alias)
    .arg(script)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .env("SSH_ASKPASS_REQUIRE", "force")
    .env("SSH_ASKPASS", std::env::current_exe()?)
    .env(askpass::NO_TTY_ENV, "1");
    if !password.is_empty() {
        cmd.env(askpass::PASSWORD_ENV, password);
    }
    let mut child = cmd.spawn()?;

    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timeout");
        }
        thread::sleep(Duration::from_millis(100));
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        s.read_to_string(&mut out)?;
    }
    if let Some(mut s) = child.stderr.take() {
        s.read_to_string(&mut err)?;
    }
    if !status.success() {
        bail!("{}", err.lines().last().unwrap_or("ssh failed").trim());
    }
    parse(&out).ok_or_else(|| anyhow::anyhow!("unexpected output"))
}

fn parse(out: &str) -> Option<SysInfo> {
    let (release, rest) = out.split_once(SEPARATOR)?;
    let field = |key: &str| {
        release.lines().find_map(|l| {
            let v = l.strip_prefix(key)?.strip_prefix('=')?;
            Some(v.trim().trim_matches('"').to_string())
        })
    };
    let os = field("PRETTY_NAME")
        .or_else(|| field("NAME"))
        .unwrap_or_default();
    let mut lines = rest.lines().map(str::trim).filter(|l| !l.is_empty());
    let hostname = lines.next()?.to_string();
    let kernel = lines.next()?.to_string();
    let arch = lines.next()?.to_string();
    let cpus = lines.next().and_then(|l| l.parse().ok());
    let mem_kb = lines.next().and_then(|l| l.parse().ok());
    Some(SysInfo {
        os,
        hostname,
        kernel,
        arch,
        cpus,
        mem_kb,
        collected: vault::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_linux() {
        let out = format!(
            "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nNAME=\"Ubuntu\"\n{SEPARATOR}\nsrv-prod\n\
             6.8.0-45-generic\nx86_64\n4\n8048152\n"
        );
        let i = parse(&out).unwrap();
        assert_eq!(i.os, "Ubuntu 24.04.1 LTS");
        assert_eq!(i.hostname, "srv-prod");
        assert_eq!(i.kernel, "6.8.0-45-generic");
        assert_eq!(i.arch, "x86_64");
        assert_eq!(i.cpus, Some(4));
        assert_eq!(i.mem_kb, Some(8048152));
    }

    #[test]
    fn parse_without_os_release_or_proc() {
        let out = format!("{SEPARATOR}\nbsd\n14.1-RELEASE\namd64\n");
        let i = parse(&out).unwrap();
        assert_eq!(i.os, "");
        assert_eq!(i.cpus, None);
        assert!(parse("garbage").is_none());
    }
}
