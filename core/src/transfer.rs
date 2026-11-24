//! Transferências de arquivos com rsync em segundo plano. O rsync passa pelo ssh com o alias
//! do ~/.ssh/sshdeck.conf, e a senha salva chega pelo SSH_ASKPASS (o próprio sshdeck).

use crate::{askpass, vault::Host};
use anyhow::{Context, Result, bail};
use rust_i18n::t;
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

/// O que o popup do rsync pede: direção e os dois caminhos.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub upload: bool,
    pub remote: String,
    pub local: String,
}

impl Request {
    /// Primeira vez: baixar para a pasta de downloads.
    pub fn to_downloads() -> Self {
        let local = dirs::download_dir()
            .and_then(|d| {
                let home = dirs::home_dir()?;
                Some(match d.strip_prefix(&home) {
                    Ok(rest) => format!("~/{}/", rest.display()),
                    Err(_) => format!("{}/", d.display()),
                })
            })
            .unwrap_or_else(|| "~/".into());
        Self {
            upload: false,
            remote: String::new(),
            local,
        }
    }
}

#[derive(Clone, Default)]
pub struct Progress {
    pub percent: u8,
    pub bytes: String,
    pub rate: String,
    pub eta: String,
}

pub struct Transfer {
    pub alias: String,
    pub upload: bool,
    pub started: Instant,
    child: Child,
    progress: Arc<Mutex<Progress>>,
    stderr: Arc<Mutex<String>>,
}

impl Transfer {
    pub fn start(host: &Host, req: &Request) -> Result<Self> {
        let remote = req.remote.trim();
        let local = req.local.trim();
        if remote.is_empty() || local.is_empty() {
            bail!("{}", t!("err.transfer_paths"));
        }
        // o rsync recebe o caminho já resolvido: sem shell, o ~ não seria expandido
        let local = expand_tilde(local);
        if req.upload && !local.exists() {
            bail!("{}", t!("err.transfer_local", path = local.display()));
        }
        if !req.upload {
            // o rsync cria a pasta final, mas não as de cima
            let dir = if req.local.trim().ends_with('/') {
                local.as_path()
            } else {
                local.parent().unwrap_or(&local)
            };
            std::fs::create_dir_all(dir).context(t!("err.transfer_mkdir"))?;
        }
        // caminho relativo no servidor = a partir do home do usuário
        let remote = format!(
            "{}:{}",
            host.alias,
            remote.strip_prefix("~/").unwrap_or(remote)
        );
        let local = local.to_string_lossy().into_owned();
        let (src, dst) = if req.upload {
            (local, remote)
        } else {
            (remote, local)
        };

        let mut child = Command::new("rsync")
            // -a: mantém permissões e datas; -z: comprime; --partial: retoma se cair;
            // --no-i-r: lista tudo antes, para a porcentagem ser do total e não do lote atual
            .args(["-azh", "--partial", "--info=progress2", "--no-i-r"])
            // nunca pergunta sobre chave nova do servidor: em segundo plano não há quem responda
            .args(["-e", "ssh -o StrictHostKeyChecking=yes"])
            .arg(src)
            .arg(dst)
            .env("LC_ALL", "C")
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env(askpass::NO_TTY_ENV, "1")
            // sempre definida: sem ela, o sshdeck chamado como askpass abriria a janela
            .env(askpass::PASSWORD_ENV, &host.password)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(t!("err.rsync_missing"))?;

        let progress = Arc::new(Mutex::new(Progress::default()));
        if let Some(mut out) = child.stdout.take() {
            let progress = progress.clone();
            thread::spawn(move || {
                // o rsync reescreve a linha de progresso com \r
                let mut line = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    for &b in &buf[..n] {
                        if b == b'\r' || b == b'\n' {
                            if let Some(p) = parse_progress(&String::from_utf8_lossy(&line)) {
                                *progress.lock().unwrap() = p;
                            }
                            line.clear();
                        } else {
                            line.push(b);
                        }
                    }
                }
            });
        }
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let stderr = stderr.clone();
            thread::spawn(move || {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                *stderr.lock().unwrap() = text;
            });
        }

        Ok(Self {
            alias: host.alias.clone(),
            upload: req.upload,
            started: Instant::now(),
            child,
            progress,
            stderr,
        })
    }

    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    /// `Some` quando terminou: `Ok` com o total transferido, ou o erro do rsync.
    pub fn poll(&mut self) -> Option<Result<String, String>> {
        let status = self.child.try_wait().ok()??;
        // a thread do stderr termina junto com o processo; espera ela entregar o texto
        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        let err = loop {
            let text = self.stderr.lock().unwrap().clone();
            if !text.is_empty() || Instant::now() > deadline {
                break text;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        };
        if status.success() {
            return Some(Ok(self.progress().bytes));
        }
        Some(Err(explain_error(&err, status.code())))
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Linha do `--info=progress2`: `  1.24G  47%   12.30MB/s    0:00:42 (xfr#3, to-chk=2/9)`.
fn parse_progress(line: &str) -> Option<Progress> {
    let mut words = line.split_whitespace();
    let bytes = words.next()?.to_string();
    let percent = words.next()?.strip_suffix('%')?.parse().ok()?;
    let rate = words.next()?.to_string();
    let eta = words.next()?.to_string();
    Some(Progress {
        percent,
        bytes,
        rate,
        eta,
    })
}

fn explain_error(stderr: &str, code: Option<i32>) -> String {
    if stderr.contains("Host key verification failed") {
        return t!("st.transfer_unknown_key").into();
    }
    if stderr.contains("rsync: command not found") || stderr.contains("rsync: not found") {
        return t!("st.transfer_no_remote_rsync").into();
    }
    // a linha útil costuma ser a primeira "rsync: ..." ou "ssh: ..."; o resto é o resumo
    stderr
        .lines()
        .map(str::trim)
        .find(|l| {
            l.starts_with("rsync:") || l.starts_with("ssh:") || l.contains("Permission denied")
        })
        .or_else(|| stderr.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(String::from)
        .unwrap_or_else(|| format!("rsync exit {}", code.unwrap_or(-1)))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None if p == "~" => dirs::home_dir().unwrap_or_default(),
        None => p.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line() {
        let p =
            parse_progress("      1.24G  47%   12.30MB/s    0:00:42 (xfr#3, to-chk=2/9)").unwrap();
        assert_eq!(p.percent, 47);
        assert_eq!(p.bytes, "1.24G");
        assert_eq!(p.rate, "12.30MB/s");
        assert_eq!(p.eta, "0:00:42");
        assert!(parse_progress("sending incremental file list").is_none());
        assert!(parse_progress("").is_none());
    }

    #[test]
    fn errors() {
        assert_eq!(
            explain_error(
                "bash: line 1: rsync: command not found\nrsync: connection unexpectedly closed (0 bytes received so far) [sender]\n",
                Some(12)
            ),
            t!("st.transfer_no_remote_rsync")
        );
        assert_eq!(
            explain_error(
                "rsync: [sender] change_dir \"/nope\" failed: No such file or directory (2)\nrsync error: some files/attrs were not transferred\n",
                Some(23)
            ),
            "rsync: [sender] change_dir \"/nope\" failed: No such file or directory (2)"
        );
        assert_eq!(explain_error("", Some(5)), "rsync exit 5");
    }
}
