//! Sessões RDP: abrem o FreeRDP numa janela própria (um terminal não desenha a área de
//! trabalho). A senha vai pelo stdin (`/from-stdin`), nunca na linha de comando.
//! Com `gateway`, primeiro sobe um túnel `ssh -L` até o servidor e o RDP passa por ele.

use crate::{askpass, vault::Host};
use anyhow::{Context, Result, bail};
use rust_i18n::t;
use std::{
    fs::{self, File},
    io::Write,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub const DEFAULT_PORT: u16 = 3389;
const CLIENTS: [&str; 4] = ["xfreerdp3", "sdl-freerdp3", "wlfreerdp3", "xfreerdp"];
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(20);

enum State {
    /// esperando o túnel ssh começar a escutar na porta local
    Tunnel {
        port: u16,
        deadline: Instant,
    },
    Running,
}

pub struct RdpSession {
    pub alias: String,
    host: Host,
    state: State,
    tunnel: Option<Child>,
    client: Option<Child>,
    log: PathBuf,
    started: Instant,
}

impl RdpSession {
    pub fn start(host: &Host, gateway: Option<&Host>) -> Result<Self> {
        if host.user.is_empty() {
            bail!("{}", t!("err.rdp_user"));
        }
        let log = log_path(&host.alias)?;
        // log só do usuário: tem IPs e nomes de usuário
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&log)?;
        fs::set_permissions(&log, fs::Permissions::from_mode(0o600))?;
        let mut s = Self {
            alias: host.alias.clone(),
            host: host.clone(),
            state: State::Running,
            tunnel: None,
            client: None,
            log,
            started: Instant::now(),
        };
        match gateway {
            Some(gw) => {
                let port = free_port()?;
                s.tunnel = Some(s.spawn_tunnel(gw, port)?);
                s.state = State::Tunnel {
                    port,
                    deadline: Instant::now() + TUNNEL_TIMEOUT,
                };
            }
            None => {
                let target = (s.host.hostname.clone(), s.host.port);
                s.client = Some(s.spawn_client(&target.0, target.1)?);
            }
        }
        Ok(s)
    }

    pub fn via_tunnel(&self) -> bool {
        self.tunnel.is_some()
    }

    pub fn connecting(&self) -> bool {
        matches!(self.state, State::Tunnel { .. })
    }

    /// Avança o estado. Devolve `Some` quando a sessão terminou: `Ok` se fechou normalmente.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        if let Some(t) = &mut self.tunnel
            && let Ok(Some(_)) = t.try_wait()
        {
            return Some(Err(t!("rdp.tunnel_down", err = self.last_error()).into()));
        }
        match self.state {
            State::Tunnel { port, deadline } => {
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                if TcpStream::connect_timeout(&addr, Duration::from_millis(30)).is_ok() {
                    self.mark("tunnel ready");
                    match self.spawn_client("127.0.0.1", port) {
                        Ok(c) => {
                            self.client = Some(c);
                            self.state = State::Running;
                        }
                        Err(e) => return Some(Err(format!("{e:#}"))),
                    }
                } else if Instant::now() > deadline {
                    return Some(Err(t!("rdp.tunnel_timeout").into()));
                }
                None
            }
            State::Running => {
                let status = self.client.as_mut()?.try_wait().ok()??;
                self.mark(&format!("FreeRDP exited ({status})"));
                // o FreeRDP usa códigos diferentes de zero até para "desconectado pelo usuário";
                // só é erro de verdade se ele registrou um ERROR no log
                let err = self.log_error();
                Some(match (status.success(), err) {
                    (false, Some(e)) => Err(e),
                    _ => Ok(()),
                })
            }
        }
    }

    fn spawn_tunnel(&self, gw: &Host, port: u16) -> Result<Child> {
        self.mark(&format!("starting tunnel via {}", gw.alias));
        let mut cmd = Command::new("ssh");
        cmd.arg("-N")
            .arg("-L")
            .arg(format!(
                "127.0.0.1:{port}:{}:{}",
                self.host.hostname, self.host.port
            ))
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
            .stderr(self.log_file()?)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env(askpass::NO_TTY_ENV, "1");
        if !gw.password.is_empty() {
            cmd.env(askpass::PASSWORD_ENV, &gw.password);
        }
        cmd.spawn().context(t!("rdp.spawn_tunnel"))
    }

    fn spawn_client(&self, server: &str, port: u16) -> Result<Child> {
        let h = &self.host;
        let mut cmd = Command::new(client_binary()?);
        cmd.arg(format!("/v:{server}:{port}"))
            .arg(format!("/u:{}", h.user))
            .arg("/from-stdin:force")
            // aceita o certificado na 1ª conexão e recusa se ele mudar depois (como o ssh)
            .arg("/cert:tofu")
            .arg("+clipboard")
            // sem papel de parede a primeira tela chega antes. Não usar /network: sem ele o
            // FreeRDP fica no autodetect, que é o que liga os codecs GFX/H.264
            .arg("-wallpaper")
            .arg(format!("/title:sshdeck · {}", h.alias))
            .arg("/log-level:ERROR");
        if !h.domain.is_empty() {
            cmd.arg(format!("/d:{}", h.domain));
        }
        match h.resolution.trim() {
            "" => cmd.args(["/size:80%", "+dynamic-resolution"]),
            r if r.eq_ignore_ascii_case("full") => cmd.arg("+f"),
            r => cmd.arg(format!("/size:{r}")),
        };
        if !h.share.is_empty() {
            cmd.arg(format!(
                "/drive:sshdeck,{}",
                expand_tilde(&h.share).display()
            ));
        }
        self.mark("starting FreeRDP");
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(self.log_file()?)
            .stderr(self.log_file()?)
            .spawn()
            .context(t!("rdp.spawn_client"))?;
        // o FreeRDP só pergunta a senha (o usuário já foi passado)
        if let Some(mut stdin) = child.stdin.take() {
            let line = Zeroizing::new(format!("{}\n", h.password));
            let _ = stdin.write_all(line.as_bytes());
        }
        Ok(child)
    }

    /// Linha de tempo no log, para ver onde a abertura demora.
    fn mark(&self, what: &str) {
        if let Ok(mut f) = self.log_file() {
            let secs = self.started.elapsed().as_secs_f32();
            let _ = writeln!(f, "[sshdeck +{secs:.2}s] {what}");
        }
    }

    fn log_file(&self) -> Result<File> {
        Ok(fs::OpenOptions::new().append(true).open(&self.log)?)
    }

    /// Última linha de ERROR do log, sem o prefixo de data/módulo.
    fn log_error(&self) -> Option<String> {
        let text = fs::read_to_string(&self.log).ok()?;
        let line = text.lines().rev().find(|l| l.contains("ERROR"))?;
        Some(match line.rsplit_once("] - ") {
            Some((_, msg)) => msg.trim().to_string(),
            None => line.trim().to_string(),
        })
    }

    fn last_error(&self) -> String {
        self.log_error()
            .or_else(|| {
                let text = fs::read_to_string(&self.log).ok()?;
                text.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.trim().to_string())
            })
            .unwrap_or_else(|| t!("rdp.see_log", path = self.log.display()).into())
    }
}

impl Drop for RdpSession {
    fn drop(&mut self) {
        for c in [self.client.as_mut(), self.tunnel.as_mut()]
            .into_iter()
            .flatten()
        {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Cliente FreeRDP: `SSHDECK_RDP_CLIENT` ou o primeiro instalado.
pub fn client_binary() -> Result<String> {
    if let Ok(c) = std::env::var("SSHDECK_RDP_CLIENT") {
        return Ok(c);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in CLIENTS {
        if std::env::split_paths(&path).any(|d| d.join(name).is_file()) {
            return Ok(name.to_string());
        }
    }
    bail!("{}", t!("rdp.not_found"))
}

/// Valida o campo de resolução: vazio, "full", "LxA" ou "N%".
pub fn valid_resolution(r: &str) -> bool {
    let r = r.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("full") {
        return true;
    }
    if let Some(p) = r.strip_suffix('%') {
        return p.parse::<u8>().is_ok_and(|p| (10..=100).contains(&p));
    }
    match r.split_once('x') {
        Some((w, h)) => w.parse::<u16>().is_ok() && h.parse::<u16>().is_ok(),
        None => false,
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => p.into(),
    }
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

fn log_path(alias: &str) -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .context(t!("rdp.no_cache"))?
        .join("sshdeck");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir.join(format!("rdp-{alias}.log")))
}

#[cfg(test)]
mod tests {
    use super::valid_resolution;

    #[test]
    fn resolution() {
        for ok in ["", "full", "FULL", "1920x1080", "90%", "10%", "100%"] {
            assert!(valid_resolution(ok), "{ok}");
        }
        for bad in ["abc", "1920", "1920x", "x1080", "5%", "101%", "90 %"] {
            assert!(!valid_resolution(bad), "{bad}");
        }
    }
}
