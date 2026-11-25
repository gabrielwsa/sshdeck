//! Navegador de arquivos (aba com duas colunas, estilo FileZilla): SFTP e FTP com o user/senha do
//! host. O SFTP roda pelo `ssh` do sistema (`ssh -s alias sftp`), então valem o `~/.ssh/config`,
//! as chaves, o ssh-agent e o known_hosts; a senha salva vai pelo askpass, como no rsync.
//! O FTP fala direto com o servidor (sem criptografia); o SMB usa só SMB2/3.
//!
//! Cópias (pastas inteiras inclusive) rodam em segundo plano e contam o progresso por um
//! `Channel`; cada cópia de FTP ou SMB abre a própria conexão para a lista não travar enquanto copia.

mod samba;

use crate::{AppState, Res, err, host_clone};
use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};
use sshdeck_core::{askpass, lang, vault::Host};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};
use suppaftp::tokio::AsyncFtpStream;
use tauri::{State, ipc::Channel};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const FTP_PORT: u16 = 21;
const CHUNK: usize = 256 * 1024;
/// quantas pastas abaixo uma cópia ou exclusão desce (proteção contra links em círculo)
const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Sftp,
    Ftp,
    Smb,
}

enum Conn {
    Sftp {
        sftp: SftpSession,
        /// o `ssh` que carrega o SFTP; morre junto com a sessão
        _child: tokio::process::Child,
    },
    Ftp(tokio::sync::Mutex<AsyncFtpStream>),
    Smb(Box<samba::Session>),
}

struct Remote {
    conn: Conn,
    host: Host,
    port: u16,
    /// Last full listing (path, entries), reused by "load more" and filter changes so a
    /// huge folder isn't fetched from the server again for each one.
    listing: Mutex<Option<(String, Arc<Vec<Entry>>)>>,
}

#[derive(Default)]
pub struct FilesState {
    remotes: Mutex<HashMap<u32, Arc<Remote>>>,
    jobs: Mutex<HashMap<u32, Arc<AtomicBool>>>,
    next: AtomicU32,
}

/// Uma linha da lista (local ou remota).
#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    name: String,
    path: String,
    dir: bool,
    link: bool,
    size: u64,
    /// segundos desde 1970
    mtime: Option<u64>,
}

#[derive(Serialize)]
pub struct Listing {
    path: String,
    entries: Vec<Entry>,
    /// quantos itens a pasta tem (com o filtro), mesmo que `entries` venha cortado
    total: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    id: u32,
    home: String,
    port: u16,
}

/// Portas efetivas e o que respondeu (`files_probe`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    sftp: bool,
    ftp: bool,
    sftp_port: u16,
    ftp_port: u16,
}

pub fn sftp_port(h: &Host) -> u16 {
    h.sftp_port.unwrap_or(h.port)
}
pub fn ftp_port(h: &Host) -> u16 {
    h.ftp_port.unwrap_or(FTP_PORT)
}

/// `pasta` + `nome` com `/` (caminhos remotos são sempre POSIX).
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn secs(t: std::time::SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn sort(entries: &mut [Entry]) {
    // Lowercases each name once instead of twice per comparison (huge folders).
    entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
}

// --- conexão ---

async fn open_sftp(host: &Host, port: u16) -> Res<Conn> {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(["-T", "-o", "ConnectTimeout=10"])
        // em segundo plano não há quem responda sobre chave nova do servidor
        .args(["-o", "StrictHostKeyChecking=yes"])
        // usa um master já aberto, mas não vira um (sairia junto com a aba)
        .args(["-o", "ControlMaster=no"]);
    if port != host.port {
        cmd.args(["-p", &port.to_string()]);
    }
    cmd.args(["-s", &host.alias, "sftp"])
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("SSH_ASKPASS", std::env::current_exe().map_err(err)?)
        .env(askpass::NO_TTY_ENV, "1")
        .env(askpass::PASSWORD_ENV, &host.password)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(err)?;
    let stdin = child.stdin.take().ok_or("stdin")?;
    let stdout = child.stdout.take().ok_or("stdout")?;
    let mut stderr = child.stderr.take().ok_or("stderr")?;
    let opened = tokio::time::timeout(
        Duration::from_secs(30),
        SftpSession::new(tokio::io::join(stdout, stdin)),
    )
    .await;
    match opened {
        Ok(Ok(sftp)) => {
            // o stderr do ssh precisa ser lido para ele não travar com o buffer cheio
            tokio::spawn(async move {
                let mut sink = Vec::new();
                let _ = stderr.read_to_end(&mut sink).await;
            });
            Ok(Conn::Sftp {
                sftp,
                _child: child,
            })
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            let last = text.lines().map(str::trim).rfind(|l| !l.is_empty());
            Err(last.map_or_else(|| e.to_string(), str::to_string))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(lang::tr("files.timeout"))
        }
    }
}

async fn ftp_connect(host: &Host, port: u16) -> Res<AsyncFtpStream> {
    let addr = tokio::net::lookup_host((host.hostname.as_str(), port))
        .await
        .map_err(err)?
        .next()
        .ok_or_else(|| format!("{}: ?", host.hostname))?;
    let mut ftp = AsyncFtpStream::connect_timeout(addr, Duration::from_secs(10))
        .await
        .map_err(err)?;
    // servidor atrás de NAT costuma anunciar o IP interno no modo passivo
    ftp.set_passive_nat_workaround(true);
    let user = if host.user.is_empty() {
        "anonymous"
    } else {
        &host.user
    };
    ftp.login(user, host.password.as_str())
        .await
        .map_err(|e| lang::tr("files.ftp_login").replace("%{err}", &e.to_string()))?;
    ftp.transfer_type(suppaftp::types::FileType::Binary)
        .await
        .map_err(err)?;
    Ok(ftp)
}

async fn open_smb(host: &Host, port: u16) -> Res<(Conn, String, u16)> {
    let session = samba::Session::connect(host, port).await?;
    Ok((Conn::Smb(Box::new(session)), "/".into(), port))
}

// --- listar ---

/// Lista a pasta. No SFTP, links aparecem como arquivos até `resolve_links` (uma consulta por
/// link: em pastas grandes isso só é feito para a parte mostrada).
async fn list_remote(r: &Remote, path: &str) -> Res<Vec<Entry>> {
    let mut out = Vec::new();
    match &r.conn {
        Conn::Sftp { sftp, .. } => {
            for e in sftp.read_dir(path).await.map_err(err)? {
                let name = e.file_name().to_string();
                let m = e.metadata();
                out.push(Entry {
                    path: join(path, &name),
                    name,
                    dir: m.is_dir(),
                    link: e.file_type().is_symlink(),
                    size: m.len(),
                    mtime: m.modified().ok().and_then(secs),
                });
            }
        }
        Conn::Ftp(_) => {
            let mut ftp = ftp_lock(r).await?;
            // MLSD é preciso (tipo, data); servidores antigos só têm LIST
            let lines = match ftp.mlsd(Some(path)).await {
                Ok(l) => l,
                Err(_) => ftp.list(Some(path)).await.map_err(err)?,
            };
            for line in lines {
                let Ok(f) = suppaftp::list::File::try_from(line.as_str()) else {
                    continue;
                };
                let name = f.name().to_string();
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                out.push(Entry {
                    path: join(path, &name),
                    name,
                    dir: f.is_directory(),
                    link: f.is_symlink(),
                    size: f.size() as u64,
                    mtime: secs(f.modified()),
                });
            }
        }
        Conn::Smb(smb) => out = smb.list(path).await?,
    }
    sort(&mut out);
    Ok(out)
}

/// Link do SFTP: tipo, tamanho e data passam a ser os do destino (se existir).
async fn resolve_links(r: &Remote, entries: &mut [Entry]) {
    let Conn::Sftp { sftp, .. } = &r.conn else {
        return;
    };
    for e in entries.iter_mut().filter(|e| e.link) {
        if let Ok(m) = sftp.metadata(e.path.clone()).await {
            e.dir = m.is_dir();
            e.size = m.len();
            e.mtime = m.modified().ok().and_then(secs);
        }
    }
}

/// Lista a pasta remota; com `cached`, reaproveita a última listagem se for da mesma pasta.
async fn cached_listing(r: &Remote, path: &str, cached: bool) -> Res<Arc<Vec<Entry>>> {
    if cached {
        let last = r.listing.lock().unwrap();
        if let Some((_, entries)) = last.as_ref().filter(|(p, _)| p == path) {
            return Ok(entries.clone());
        }
    }
    let entries = Arc::new(list_remote(r, path).await?);
    *r.listing.lock().unwrap() = Some((path.to_owned(), entries.clone()));
    Ok(entries)
}

/// A conexão FTP da aba, reconectando se ela caiu (servidores FTP derrubam conexões paradas
/// depois de alguns minutos).
async fn ftp_lock(r: &Remote) -> Res<tokio::sync::MutexGuard<'_, AsyncFtpStream>> {
    let Conn::Ftp(ftp) = &r.conn else {
        return Err("ftp".into());
    };
    let mut guard = ftp.lock().await;
    if guard.noop().await.is_err() {
        *guard = ftp_connect(&r.host, r.port).await?;
    }
    Ok(guard)
}

fn list_local(path: &Path) -> Res<Vec<Entry>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(path).map_err(err)? {
        let Ok(e) = e else { continue };
        let full = e.path();
        let link = e.file_type().is_ok_and(|t| t.is_symlink());
        // segue o link; link quebrado aparece como arquivo de tamanho 0
        let meta = std::fs::metadata(&full).or_else(|_| std::fs::symlink_metadata(&full));
        let Ok(meta) = meta else { continue };
        out.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            path: full.to_string_lossy().into_owned(),
            dir: meta.is_dir(),
            link,
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: meta.modified().ok().and_then(secs),
        });
    }
    sort(&mut out);
    Ok(out)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn local_path(p: &str) -> PathBuf {
    let p = p.trim();
    if p.is_empty() || p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

// --- comandos ---

/// Quais protocolos respondem no servidor (só a porta TCP; o login é testado ao abrir).
#[tauri::command]
pub async fn files_probe(state: State<'_, AppState>, alias: String) -> Res<Probe> {
    let h = host_clone(&state, &alias)?;
    let (sp, fp) = (sftp_port(&h), ftp_port(&h));
    let open = |port: u16| {
        let host = h.hostname.clone();
        async move {
            matches!(
                tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::net::TcpStream::connect((host.as_str(), port))
                )
                .await,
                Ok(Ok(_))
            )
        }
    };
    let (sftp, ftp) = tokio::join!(open(sp), open(fp));
    Ok(Probe {
        sftp,
        ftp,
        sftp_port: sp,
        ftp_port: fp,
    })
}

#[tauri::command]
pub async fn files_open(
    state: State<'_, AppState>,
    files: State<'_, FilesState>,
    alias: String,
    proto: Proto,
) -> Res<Opened> {
    let host = host_clone(&state, &alias)?;
    let (conn, home, port) = match proto {
        Proto::Sftp => {
            let port = sftp_port(&host);
            let conn = open_sftp(&host, port).await?;
            let Conn::Sftp { sftp, .. } = &conn else {
                unreachable!()
            };
            let home = sftp.canonicalize(".").await.map_err(err)?.to_string();
            (conn, home, port)
        }
        Proto::Ftp => {
            let port = ftp_port(&host);
            let mut ftp = ftp_connect(&host, port).await?;
            let home = ftp.pwd().await.unwrap_or_else(|_| "/".into());
            (Conn::Ftp(tokio::sync::Mutex::new(ftp)), home, port)
        }
        Proto::Smb => open_smb(&host, samba::PORT).await?,
    };
    let id = files.next.fetch_add(1, Ordering::Relaxed) + 1;
    files.remotes.lock().unwrap().insert(
        id,
        Arc::new(Remote {
            conn,
            host,
            port,
            listing: Mutex::default(),
        }),
    );
    Ok(Opened { id, home, port })
}

#[tauri::command]
pub async fn files_close(files: State<'_, FilesState>, id: u32) -> Res<()> {
    let r = files.remotes.lock().unwrap().remove(&id);
    if let Some(r) = r {
        match &r.conn {
            Conn::Sftp { sftp, .. } => {
                let _ = sftp.close().await;
            }
            Conn::Ftp(ftp) => {
                let _ = ftp.lock().await.quit().await;
            }
            Conn::Smb(_) => {}
        }
    }
    Ok(())
}

fn remote(files: &FilesState, id: u32) -> Res<Arc<Remote>> {
    files
        .remotes
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| lang::tr("files.closed"))
}

/// Lista uma pasta. `id = None` é o computador local (`~` = home).
#[tauri::command]
pub async fn files_list(
    files: State<'_, FilesState>,
    id: Option<u32>,
    path: String,
    limit: Option<usize>,
    filter: Option<String>,
    hidden: Option<bool>,
    cached: Option<bool>,
) -> Res<Listing> {
    let (path, mut entries, r) = match id {
        None => {
            let p = local_path(&path);
            let p = std::fs::canonicalize(&p).unwrap_or(p);
            let entries = list_local(&p)?;
            (p.to_string_lossy().into_owned(), entries, None)
        }
        Some(id) => {
            let r = remote(&files, id)?;
            // SFTP: o caminho canônico resolve `..` e links
            let path = match &r.conn {
                Conn::Sftp { sftp, .. } => sftp
                    .canonicalize(path.clone())
                    .await
                    .map(|p| p.to_string())
                    .unwrap_or(path),
                Conn::Ftp(_) => normalize(&path),
                Conn::Smb(_) => samba::clean(&path)?,
            };
            let entries = cached_listing(&r, &path, cached.unwrap_or(false)).await?;
            (path, Vec::clone(&entries), Some(r))
        }
    };
    let total = shape(
        &mut entries,
        limit,
        filter.as_deref(),
        hidden.unwrap_or(false),
    );
    if let Some(r) = r {
        resolve_links(&r, &mut entries).await;
        sort(&mut entries);
    }
    Ok(Listing {
        path,
        entries,
        total,
    })
}

/// Tira os ocultos (se pedido), filtra pelo nome e corta em `limit`. Devolve quantos passaram no
/// filtro (antes do corte), para a janela mostrar "500 de 3200".
fn shape(
    entries: &mut Vec<Entry>,
    limit: Option<usize>,
    filter: Option<&str>,
    hidden: bool,
) -> usize {
    let needle = filter
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_lowercase);
    entries.retain(|e| {
        (hidden || !e.name.starts_with('.'))
            && needle
                .as_ref()
                .is_none_or(|n| e.name.to_lowercase().contains(n.as_str()))
    });
    let total = entries.len();
    if let Some(l) = limit.filter(|l| *l > 0) {
        entries.truncate(l);
    }
    total
}

/// Quais destes nomes já existem na pasta (aviso antes de sobrescrever; a lista na tela pode
/// estar cortada pelo limite).
#[tauri::command]
pub async fn files_existing(
    files: State<'_, FilesState>,
    id: Option<u32>,
    dir: String,
    names: Vec<String>,
) -> Res<Vec<String>> {
    match id {
        None => {
            let d = local_path(&dir);
            Ok(names
                .into_iter()
                .filter(|n| d.join(n).symlink_metadata().is_ok())
                .collect())
        }
        Some(id) => {
            let r = remote(&files, id)?;
            existing_remote(&r, &dir, names).await
        }
    }
}

async fn existing_remote(r: &Remote, dir: &str, names: Vec<String>) -> Res<Vec<String>> {
    if let Conn::Smb(smb) = &r.conn {
        return smb.existing(dir, names).await;
    }
    let present: Vec<_> = list_remote(r, dir)
        .await?
        .into_iter()
        .map(|e| e.name)
        .collect();
    Ok(names.into_iter().filter(|n| present.contains(n)).collect())
}

/// Resolve `.` e `..` num caminho POSIX absoluto (o FTP não tem canonicalize).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

#[tauri::command]
pub async fn files_mkdir(files: State<'_, FilesState>, id: Option<u32>, path: String) -> Res<()> {
    match id {
        None => std::fs::create_dir(local_path(&path)).map_err(err),
        Some(id) => {
            let r = remote(&files, id)?;
            mkdir_remote(&r, &path).await
        }
    }
}

async fn mkdir_remote(r: &Remote, path: &str) -> Res<()> {
    match &r.conn {
        Conn::Sftp { sftp, .. } => sftp.create_dir(path).await.map_err(err),
        Conn::Ftp(_) => ftp_lock(r).await?.mkdir(path).await.map_err(err),
        Conn::Smb(smb) => smb.mkdir(path, false).await,
    }
}

#[tauri::command]
pub async fn files_rename(
    files: State<'_, FilesState>,
    id: Option<u32>,
    from: String,
    to: String,
) -> Res<()> {
    match id {
        None => std::fs::rename(local_path(&from), local_path(&to)).map_err(err),
        Some(id) => {
            let r = remote(&files, id)?;
            rename_remote(&r, &from, &to).await
        }
    }
}

async fn rename_remote(r: &Remote, from: &str, to: &str) -> Res<()> {
    match &r.conn {
        Conn::Sftp { sftp, .. } => sftp.rename(from, to).await.map_err(err),
        Conn::Ftp(_) => ftp_lock(r).await?.rename(from, to).await.map_err(err),
        Conn::Smb(smb) => smb.rename(from, to).await,
    }
}

#[derive(Deserialize, Clone)]
pub struct Item {
    path: String,
    dir: bool,
}

/// Apaga arquivos e pastas (com tudo dentro).
#[tauri::command]
pub async fn files_remove(
    files: State<'_, FilesState>,
    id: Option<u32>,
    items: Vec<Item>,
) -> Res<()> {
    match id {
        None => {
            for it in items {
                let p = local_path(&it.path);
                // link para pasta: apaga o link, não o que ele aponta
                let is_link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_symlink());
                if it.dir && !is_link {
                    std::fs::remove_dir_all(&p).map_err(err)?;
                } else {
                    std::fs::remove_file(&p).map_err(err)?;
                }
            }
            Ok(())
        }
        Some(id) => {
            let r = remote(&files, id)?;
            for it in items {
                remove_remote(&r, &it.path, it.dir, 0).await?;
            }
            Ok(())
        }
    }
}

async fn remove_remote(r: &Remote, path: &str, dir: bool, depth: usize) -> Res<()> {
    if let Conn::Smb(smb) = &r.conn {
        return smb.remove(path).await;
    }
    if dir && depth < MAX_DEPTH {
        for e in Box::pin(list_remote(r, path)).await? {
            // link para pasta: apaga só o link
            Box::pin(remove_remote(r, &e.path, e.dir && !e.link, depth + 1)).await?;
        }
    }
    match &r.conn {
        Conn::Sftp { sftp, .. } => {
            if dir {
                sftp.remove_dir(path).await.map_err(err)
            } else {
                sftp.remove_file(path).await.map_err(err)
            }
        }
        Conn::Ftp(_) => {
            let mut ftp = ftp_lock(r).await?;
            if dir {
                ftp.rmdir(path).await.map_err(err)
            } else {
                ftp.rm(path).await.map_err(err)
            }
        }
        Conn::Smb(_) => unreachable!(),
    }
}

// --- copiar ---

/// Progresso de uma cópia (para a janela).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum CopyEvent {
    Progress {
        done: u64,
        total: u64,
        file: String,
        files_done: usize,
        files_total: usize,
        rate: u64,
    },
    Finished {
        error: Option<String>,
        cancelled: bool,
    },
}

/// O que copiar: um arquivo (ou pasta a criar) com origem, destino e tamanho.
struct Step {
    src: String,
    dst: String,
    dir: bool,
    size: u64,
}

async fn plan_download(
    r: &Remote,
    item: &Item,
    dst_dir: &Path,
    steps: &mut Vec<Step>,
    depth: usize,
    cancel: &AtomicBool,
) -> Res<()> {
    check_cancel(cancel)?;
    let name = item
        .path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("arquivo");
    let dst = dst_dir.join(name);
    if item.dir {
        steps.push(Step {
            src: item.path.clone(),
            dst: dst.to_string_lossy().into_owned(),
            dir: true,
            size: 0,
        });
        if depth >= MAX_DEPTH {
            return Ok(());
        }
        let mut list = Box::pin(list_remote(r, &item.path)).await?;
        resolve_links(r, &mut list).await;
        for e in list {
            let child = Item {
                path: e.path.clone(),
                dir: e.dir && !e.link,
            };
            if e.link && (e.dir || matches!(r.conn, Conn::Smb(_))) {
                continue; // link para pasta: pode dar volta, fica de fora
            }
            if e.dir {
                Box::pin(plan_download(r, &child, &dst, steps, depth + 1, cancel)).await?;
            } else {
                steps.push(Step {
                    src: e.path,
                    dst: dst.join(&e.name).to_string_lossy().into_owned(),
                    dir: false,
                    size: e.size,
                });
            }
        }
    } else {
        let size = match &r.conn {
            Conn::Sftp { sftp, .. } => sftp
                .metadata(item.path.clone())
                .await
                .map(|m| m.len())
                .unwrap_or(0),
            Conn::Ftp(_) => ftp_lock(r)
                .await?
                .size(&item.path)
                .await
                .map(|s| s as u64)
                .unwrap_or(0),
            Conn::Smb(smb) => smb.download_size(&item.path, false).await?,
        };
        steps.push(Step {
            src: item.path.clone(),
            dst: dst.to_string_lossy().into_owned(),
            dir: false,
            size,
        });
    }
    Ok(())
}

fn plan_upload(src: &Path, dst_dir: &str, steps: &mut Vec<Step>, depth: usize) -> Res<()> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dst = join(dst_dir, &name);
    let meta = std::fs::metadata(src).map_err(err)?;
    if meta.is_dir() {
        steps.push(Step {
            src: src.to_string_lossy().into_owned(),
            dst: dst.clone(),
            dir: true,
            size: 0,
        });
        if depth >= MAX_DEPTH {
            return Ok(());
        }
        let mut children: Vec<_> = std::fs::read_dir(src).map_err(err)?.flatten().collect();
        children.sort_by_key(|e| e.file_name());
        for e in children {
            let p = e.path();
            // link para pasta: pode dar volta, fica de fora
            if e.file_type().is_ok_and(|t| t.is_symlink()) && p.is_dir() {
                continue;
            }
            plan_upload(&p, &dst, steps, depth + 1)?;
        }
    } else {
        steps.push(Step {
            src: src.to_string_lossy().into_owned(),
            dst,
            dir: false,
            size: meta.len(),
        });
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Res<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(String::new())
    } else {
        Ok(())
    }
}

/// Copia `reader` → `writer` contando o progresso.
async fn pump(
    mut reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    prog: &mut Progress<'_>,
) -> Res<()> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        check_cancel(prog.cancel)?;
        let n = reader.read(&mut buf).await.map_err(err)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await.map_err(err)?;
        prog.add(n as u64);
    }
    writer.flush().await.map_err(err)?;
    Ok(())
}

struct Progress<'a> {
    channel: &'a Channel<CopyEvent>,
    cancel: &'a AtomicBool,
    done: u64,
    total: u64,
    file: String,
    files_done: usize,
    files_total: usize,
    started: Instant,
    last: Instant,
}

impl Progress<'_> {
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() >= Duration::from_millis(150) {
            self.send();
        }
    }

    fn send(&mut self) {
        self.last = Instant::now();
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let _ = self.channel.send(CopyEvent::Progress {
            done: self.done,
            total: self.total,
            file: self.file.clone(),
            files_done: self.files_done,
            files_total: self.files_total,
            rate: (self.done as f64 / secs) as u64,
        });
    }
}

async fn run_copy(
    r: Arc<Remote>,
    upload: bool,
    items: Vec<Item>,
    dest: String,
    channel: &Channel<CopyEvent>,
    cancel: &AtomicBool,
) -> Res<()> {
    check_cancel(cancel)?;
    // FTP e SMB: conexão própria para a cópia (a da aba continua livre para listar).
    let own_ftp = match &r.conn {
        Conn::Ftp(_) => Some(tokio::sync::Mutex::new(ftp_connect(&r.host, r.port).await?)),
        Conn::Smb(_) | Conn::Sftp { .. } => None,
    };
    let own_smb = match &r.conn {
        Conn::Smb(smb) => Some(smb.fork()),
        _ => None,
    };

    let mut steps = Vec::new();
    if upload {
        let dest = if own_smb.is_some() {
            let dest = samba::clean(&dest)?;
            if dest == "/" {
                return Err("SMB: select a disk share for uploads".into());
            }
            dest
        } else {
            dest
        };
        for it in &items {
            check_cancel(cancel)?;
            plan_upload(&local_path(&it.path), &dest, &mut steps, 0)?;
        }
    } else {
        let dst = local_path(&dest);
        for mut it in items {
            check_cancel(cancel)?;
            if let Some(smb) = &own_smb {
                it.path = samba::clean(&it.path)?;
                if it.dir {
                    smb.download_size(&it.path, true).await?;
                }
            }
            plan_download(&r, &it, &dst, &mut steps, 0, cancel).await?;
        }
    }
    let mut prog = Progress {
        channel,
        cancel,
        done: 0,
        total: steps.iter().map(|s| s.size).sum(),
        file: String::new(),
        files_done: 0,
        files_total: steps.iter().filter(|s| !s.dir).count(),
        started: Instant::now(),
        last: Instant::now(),
    };
    prog.send();

    for step in &steps {
        check_cancel(cancel)?;
        prog.file = step.src.rsplit('/').next().unwrap_or_default().to_string();
        if step.dir {
            // pasta que já existe não é erro
            if upload {
                if let Some(smb) = &own_smb {
                    smb.mkdir(&step.dst, true).await?;
                    continue;
                }
                let exists = match (&r.conn, &own_ftp) {
                    (Conn::Sftp { sftp, .. }, _) => {
                        sftp.try_exists(step.dst.clone()).await.unwrap_or(false)
                    }
                    (_, Some(ftp)) => ftp.lock().await.cwd(&step.dst).await.is_ok(),
                    _ => false,
                };
                if !exists {
                    match (&r.conn, &own_ftp) {
                        (Conn::Sftp { sftp, .. }, _) => {
                            sftp.create_dir(step.dst.clone()).await.map_err(err)?
                        }
                        (_, Some(ftp)) => ftp.lock().await.mkdir(&step.dst).await.map_err(err)?,
                        _ => {}
                    }
                }
            } else {
                std::fs::create_dir_all(&step.dst).map_err(err)?;
            }
            continue;
        }
        match (&r.conn, &own_ftp, &own_smb, upload) {
            (Conn::Smb(_), _, Some(smb), _) => smb.copy_file(step, upload, &mut prog).await?,
            (Conn::Sftp { sftp, .. }, _, _, false) => {
                let src = sftp.open(step.src.clone()).await.map_err(err)?;
                let dst = tokio::fs::File::create(&step.dst).await.map_err(err)?;
                pump(src, dst, &mut prog).await?;
            }
            (Conn::Sftp { sftp, .. }, _, _, true) => {
                let src = tokio::fs::File::open(&step.src).await.map_err(err)?;
                let mut dst = sftp.create(step.dst.clone()).await.map_err(err)?;
                pump(src, &mut dst, &mut prog).await?;
                dst.shutdown().await.map_err(err)?;
            }
            (_, Some(ftp), _, false) => {
                let mut ftp = ftp.lock().await;
                let mut src = ftp.retr_as_stream(&step.src).await.map_err(err)?;
                let dst = tokio::fs::File::create(&step.dst).await.map_err(err)?;
                pump(&mut src, dst, &mut prog).await?;
                src.finish().await.map_err(err)?;
            }
            (_, Some(ftp), _, true) => {
                let mut ftp = ftp.lock().await;
                let src = tokio::fs::File::open(&step.src).await.map_err(err)?;
                let mut dst = ftp.put_with_stream(&step.dst).await.map_err(err)?;
                pump(src, &mut dst, &mut prog).await?;
                dst.finish().await.map_err(err)?;
            }
            _ => {}
        }
        prog.files_done += 1;
        prog.send();
    }
    if let Some(ftp) = own_ftp {
        let _ = ftp.lock().await.quit().await;
    }
    Ok(())
}

/// Começa uma cópia. `upload`: local → servidor; senão servidor → local. Devolve o id da cópia.
#[tauri::command]
pub fn files_copy(
    files: State<'_, FilesState>,
    id: u32,
    upload: bool,
    items: Vec<Item>,
    dest: String,
    on_event: Channel<CopyEvent>,
) -> Res<u32> {
    let r = remote(&files, id)?;
    let job = files.next.fetch_add(1, Ordering::Relaxed) + 1;
    let cancel = Arc::new(AtomicBool::new(false));
    files.jobs.lock().unwrap().insert(job, cancel.clone());
    tauri::async_runtime::spawn(async move {
        let result = run_copy(r, upload, items, dest, &on_event, &cancel).await;
        let cancelled = cancel.load(Ordering::Relaxed);
        let _ = on_event.send(CopyEvent::Finished {
            error: match result {
                Err(e) if !cancelled && !e.is_empty() => Some(e),
                _ => None,
            },
            cancelled,
        });
    });
    Ok(job)
}

#[tauri::command]
pub fn files_cancel(files: State<'_, FilesState>, job: u32) {
    if let Some(c) = files.jobs.lock().unwrap().remove(&job) {
        c.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smb_ipc_protocol_is_lowercase() {
        use serde::de::value::{Error, StrDeserializer};

        assert!(Proto::deserialize(StrDeserializer::<Error>::new("smb")).is_ok());
        assert!(Proto::deserialize(StrDeserializer::<Error>::new("SMB")).is_err());
    }

    // Run only against a disposable Samba fixture on the loopback interface.
    async fn smb_fixture() -> Remote {
        let port = std::env::var("SSHDECK_SMB_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let host = Host {
            hostname: "127.0.0.1".into(),
            user: r"SSHDECK\fixture".into(),
            password: std::env::var("SSHDECK_SMB_PASSWORD").unwrap(),
            port: 3389,
            ..Host::default()
        };
        let (conn, home, actual_port) = open_smb(&host, port).await.unwrap();
        assert_eq!(home, "/");
        assert_eq!(actual_port, port);
        Remote {
            conn,
            host,
            port,
            listing: Mutex::default(),
        }
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_lists_disk_shares() {
        let r = smb_fixture().await;
        let root = list_remote(&r, "/").await.unwrap();
        let shares: Vec<_> = root.iter().map(|e| (e.path.as_str(), e.dir)).collect();
        assert_eq!(shares, [("/files", true), ("/more", true)]);
        let entries = list_remote(&r, "/files/").await.unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.name == "seed.txt" && !e.dir && e.size == 8 && e.mtime.is_some())
        );
    }

    // Windows deduplication and cloud-file placeholders set FILE_ATTRIBUTE_REPARSE_POINT on
    // ordinary data files. Only link tags are refused; data tags are opened without
    // FILE_OPEN_REPARSE_POINT so the server's filter driver supplies the contents. Samba has no
    // such filter and answers STATUS_IO_REPARSE_TAG_NOT_HANDLED, like a Windows server
    // without the dedup role, which proves the client asked for the data, not the stub.
    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_treats_data_reparse_files_as_regular() {
        let r = Arc::new(smb_fixture().await);
        let Conn::Smb(smb) = &r.conn else {
            unreachable!()
        };
        let pid = std::process::id();
        let temp = std::env::temp_dir().join(format!("sshdeck-smb-reparse-{pid}"));
        std::fs::create_dir_all(&temp).unwrap();
        let dir = format!("/files/reparse-{pid}");
        mkdir_remote(&r, &dir).await.unwrap();
        let events = Channel::new(|_| Ok(()));
        let cancel = AtomicBool::new(false);
        let tags = [
            ("cloud.png", 0x9000_101a_u32),
            ("dedup.png", 0x8000_0013),
            ("wsl-link", 0xa000_001d),
        ];
        for (name, tag) in tags {
            let local = temp.join(name);
            std::fs::write(&local, name.as_bytes()).unwrap();
            let item = Item {
                path: local.to_string_lossy().into_owned(),
                dir: false,
            };
            run_copy(r.clone(), true, vec![item], dir.clone(), &events, &cancel)
                .await
                .unwrap();
            smb.set_reparse_tag(&join(&dir, name), tag).await.unwrap();
        }
        let entries = list_remote(&r, &dir).await.unwrap();
        let kinds: Vec<_> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.dir, e.link))
            .collect();
        assert_eq!(
            kinds,
            [
                ("cloud.png", false, false),
                ("dedup.png", false, false),
                ("wsl-link", false, true)
            ]
        );
        for (name, tag) in tags {
            let item = Item {
                path: join(&dir, name),
                dir: false,
            };
            let down = run_copy(
                r.clone(),
                false,
                vec![item],
                temp.join("down").to_string_lossy().into_owned(),
                &events,
                &cancel,
            )
            .await
            .unwrap_err();
            let local = temp.join(name);
            let up = run_copy(
                r.clone(),
                true,
                vec![Item {
                    path: local.to_string_lossy().into_owned(),
                    dir: false,
                }],
                dir.clone(),
                &events,
                &cancel,
            )
            .await
            .unwrap_err();
            for e in [down, up] {
                if tag == 0xa000_001d {
                    assert!(e.contains("refusing to follow a reparse point"), "{e}");
                } else {
                    assert!(e.contains(&format!("reparse tag {tag:#x}")), "{e}");
                }
            }
        }
        remove_remote(&r, &dir, true, 0).await.unwrap();
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_reuses_listing_only_when_asked() {
        let r = smb_fixture().await;
        let dir = format!("/files/cache-{}", std::process::id());
        mkdir_remote(&r, &dir).await.unwrap();
        let first = cached_listing(&r, &dir, false).await.unwrap();
        assert!(first.is_empty());
        mkdir_remote(&r, &join(&dir, "child")).await.unwrap();
        // "Load more", filter and option changes reuse the last listing of the same folder.
        let again = cached_listing(&r, &dir, true).await.unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let fresh = cached_listing(&r, &dir, false).await.unwrap();
        assert_eq!(fresh.len(), 1);
        let other = cached_listing(&r, "/files", true).await.unwrap();
        assert!(other.iter().any(|e| e.name == "seed.txt"));
        assert_eq!(cached_listing(&r, &dir, true).await.unwrap().len(), 1);
        remove_remote(&r, &dir, true, 0).await.unwrap();
    }

    // Needs `files/big` with 120 000 empty `IMG_nnnnnn.png` files. Prints timings.
    #[tokio::test]
    #[ignore = "requires the disposable Samba fixture with a huge directory"]
    async fn smbbig_fixture_lists_huge_directory() {
        let r = smb_fixture().await;
        for round in 0..3 {
            let start = std::time::Instant::now();
            let mut entries = list_remote(&r, "/files/big").await.unwrap();
            let listed = start.elapsed();
            let total = shape(&mut entries, Some(500), None, false);
            resolve_links(&r, &mut entries).await;
            sort(&mut entries);
            eprintln!(
                "round {round}: list_remote {listed:?}, files_list total {:?}",
                start.elapsed()
            );
            assert_eq!(total, 120_000);
            assert_eq!(entries[0].name, "IMG_000000.png");
            assert!(entries.iter().all(|e| !e.link && !e.dir));
        }
        cached_listing(&r, "/files/big", false).await.unwrap();
        let start = std::time::Instant::now();
        let mut entries = Vec::clone(&cached_listing(&r, "/files/big", true).await.unwrap());
        let total = shape(&mut entries, Some(1000), Some("img_0999"), false);
        eprintln!("cached (load more / filter): {:?}", start.elapsed());
        assert_eq!(total, 100);
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_reads_when_both_handle_name_queries_are_unsupported() {
        let r = Arc::new(smb_fixture().await);
        let temp = std::env::temp_dir().join(format!("sshdeck-smb-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        samba::LEGACY_NAME_STATUS
            .scope((), async {
                let dir = "/files/symlink-target";
                let entries = list_remote(&r, dir).await.unwrap();
                assert!(entries.iter().any(|e| e.name == "keep"));
                run_copy(
                    r.clone(),
                    false,
                    vec![Item {
                        path: join(dir, "keep"),
                        dir: false,
                    }],
                    temp.to_string_lossy().into_owned(),
                    &Channel::new(|_| Ok(())),
                    &AtomicBool::new(false),
                )
                .await
                .unwrap();
            })
            .await;
        assert_eq!(std::fs::read(temp.join("keep")).unwrap(), b"safe");
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_uploads_new_root_file_without_handle_names() {
        let r = Arc::new(smb_fixture().await);
        let temp =
            std::env::temp_dir().join(format!("sshdeck-smb-legacy-write-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let source = temp.join(format!("new-{}", std::process::id()));
        std::fs::write(&source, b"initial contents").unwrap();
        let item = Item {
            path: source.to_string_lossy().into_owned(),
            dir: false,
        };
        let channel = Channel::new(|_| Ok(()));
        let cancel = AtomicBool::new(false);
        samba::LEGACY_NAME_STATUS
            .scope((), async {
                run_copy(
                    r.clone(),
                    true,
                    vec![item.clone()],
                    "/files".into(),
                    &channel,
                    &cancel,
                )
                .await
                .unwrap();
                std::fs::write(&source, b"must not overwrite").unwrap();
                let result = run_copy(
                    r.clone(),
                    true,
                    vec![item],
                    "/files".into(),
                    &channel,
                    &cancel,
                )
                .await;
                assert!(result.unwrap_err().contains("refusing to modify"));
            })
            .await;
        let remote = join("/files", source.file_name().unwrap().to_str().unwrap());
        run_copy(
            r.clone(),
            false,
            vec![Item {
                path: remote.clone(),
                dir: false,
            }],
            temp.to_string_lossy().into_owned(),
            &channel,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(source).unwrap(), b"initial contents");
        let dangling = temp.join("root-dangling");
        std::fs::write(&dangling, b"must not redirect creation").unwrap();
        let result = samba::LEGACY_NAME_STATUS
            .scope(
                (),
                run_copy(
                    r.clone(),
                    true,
                    vec![Item {
                        path: dangling.to_string_lossy().into_owned(),
                        dir: false,
                    }],
                    "/files".into(),
                    &channel,
                    &cancel,
                ),
            )
            .await;
        assert!(result.is_err());
        let target = list_remote(&r, "/files/symlink-target").await.unwrap();
        assert!(!target.iter().any(|e| e.name == "not-created"));
        remove_remote(&r, &remote, false, 0).await.unwrap();
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_manages_directories() {
        let r = smb_fixture().await;
        let parent = format!("/files/dirs-{}", std::process::id());
        assert!(remove_remote(&r, "/files", true, 0).await.is_err());
        mkdir_remote(&r, &parent).await.unwrap();
        let from = join(&parent, "New Folder");
        let to = join(&parent, "Renamed á");
        mkdir_remote(&r, &from).await.unwrap();
        assert!(rename_remote(&r, &from, "/more/blocked").await.is_err());
        rename_remote(&r, &from, &to).await.unwrap();
        assert!(list_remote(&r, &from).await.is_err());
        assert!(list_remote(&r, &to).await.unwrap().is_empty());
        let names = existing_remote(&r, &parent, vec!["RENAMED Á".into(), "missing".into()])
            .await
            .unwrap();
        assert_eq!(names, ["RENAMED Á"]);
        remove_remote(&r, &parent, true, 0).await.unwrap();
        assert!(list_remote(&r, &parent).await.is_err());
        assert!(
            list_remote(&r, "/files")
                .await
                .unwrap()
                .iter()
                .any(|e| e.name == "seed.txt")
        );
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_copies_tree_both_ways() {
        let r = Arc::new(smb_fixture().await);
        let temp = std::env::temp_dir().join(format!("sshdeck-smb-{}", std::process::id()));
        let dir_name = format!("upload-{}", std::process::id());
        let src = temp.join(&dir_name);
        let dst = temp.join("download");
        std::fs::create_dir_all(src.join("Sub Pasta")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let expected: Vec<u8> = (0..CHUNK * 5 + 7).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("Sub Pasta/arquivo á.bin"), &expected).unwrap();
        std::fs::write(src.join("empty"), []).unwrap();
        std::fs::write(src.join("shrink"), b"original contents").unwrap();
        let events = Channel::new(|_| Ok(()));
        let cancel = AtomicBool::new(false);
        let local = Item {
            path: src.to_string_lossy().into_owned(),
            dir: true,
        };
        let remote = Item {
            path: join("/files", &dir_name),
            dir: true,
        };
        run_copy(
            r.clone(),
            true,
            vec![local.clone()],
            "/files".into(),
            &events,
            &cancel,
        )
        .await
        .unwrap();
        // Upload again, merging directories and truncating a file to a shorter size.
        std::fs::write(src.join("shrink"), b"hi").unwrap();
        run_copy(
            r.clone(),
            true,
            vec![local],
            "/files".into(),
            &events,
            &cancel,
        )
        .await
        .unwrap();
        run_copy(
            r.clone(),
            false,
            vec![remote.clone()],
            dst.to_string_lossy().into_owned(),
            &events,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(dst.join(&dir_name).join("Sub Pasta/arquivo á.bin")).unwrap(),
            expected
        );
        assert!(
            std::fs::read(dst.join(&dir_name).join("empty"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read(dst.join(&dir_name).join("shrink")).unwrap(),
            b"hi"
        );
        remove_remote(&r, &remote.path, remote.dir, 0)
            .await
            .unwrap();
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the disposable Samba symlink fixture"]
    async fn smb_symlink_fixture_never_removes_the_target() {
        let r = smb_fixture().await;
        let before = list_remote(&r, "/files/symlink-target").await.unwrap();
        assert!(before.iter().any(|e| e.name == "keep"));
        let _ = remove_remote(&r, "/files/symlink-probe", true, 0).await;
        let after = list_remote(&r, "/files/symlink-target").await.unwrap();
        assert!(after.iter().any(|e| e.name == "keep"));
    }

    #[tokio::test]
    #[ignore = "requires the disposable Samba symlink fixture"]
    async fn smb_symlink_fixture_never_overwrites_the_target() {
        let r = Arc::new(smb_fixture().await);
        let temp = std::env::temp_dir().join(format!("sshdeck-smb-link-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let source = temp.join("keep");
        std::fs::write(&source, b"unexpected overwrite").unwrap();
        let item = Item {
            path: source.to_string_lossy().into_owned(),
            dir: false,
        };
        let cancel = AtomicBool::new(false);
        let events = Channel::new(|_| Ok(()));
        let result = run_copy(
            r.clone(),
            true,
            vec![item],
            "/files/symlink-probe/shortcut".into(),
            &events,
            &cancel,
        )
        .await;
        let target = list_remote(&r, "/files/symlink-target").await.unwrap();
        std::fs::remove_dir_all(temp).unwrap();
        assert!(target.iter().any(|e| e.name == "keep" && e.size == 4));
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires the disposable loopback Samba fixture"]
    async fn smb_fixture_honors_cancel_before_copy_writes() {
        let r = Arc::new(smb_fixture().await);
        let dst = std::env::temp_dir().join(format!("sshdeck-smb-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dst).unwrap();
        let item = Item {
            path: "/files/seed.txt".into(),
            dir: false,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let events = Channel::new(move |_| {
            flag.store(true, Ordering::Relaxed);
            Ok(())
        });
        let result = run_copy(
            r,
            false,
            vec![item],
            dst.to_string_lossy().into_owned(),
            &events,
            &cancel,
        )
        .await;
        assert_eq!(result, Err(String::new()));
        assert!(!dst.join("seed.txt").exists());
        std::fs::remove_dir_all(dst).unwrap();
    }

    #[test]
    fn paths() {
        assert_eq!(join("/home/a", "b"), "/home/a/b");
        assert_eq!(join("/", "b"), "/b");
        assert_eq!(normalize("/a/b/../c/./d/"), "/a/c/d");
        assert_eq!(normalize("/.."), "/");
        assert_eq!(local_path("~/x"), home().join("x"));
    }

    #[test]
    fn sort_puts_folders_first_ignoring_case() {
        let e = |n: &str, dir: bool| Entry {
            name: n.into(),
            path: format!("/x/{n}"),
            dir,
            link: false,
            size: 0,
            mtime: None,
        };
        let mut v = vec![
            e("b", false),
            e("Zeta", true),
            e("A", false),
            e("alfa", true),
        ];
        sort(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alfa", "Zeta", "A", "b"]);
    }

    #[test]
    fn shape_filters_and_limits() {
        let e = |n: &str| Entry {
            name: n.into(),
            path: format!("/x/{n}"),
            dir: false,
            link: false,
            size: 0,
            mtime: None,
        };
        let all = || {
            vec![
                e(".oculto"),
                e("app.log"),
                e("APP.conf"),
                e("dados.csv"),
                e("z.log"),
            ]
        };
        let mut v = all();
        assert_eq!(shape(&mut v, Some(2), None, false), 4);
        assert_eq!(v.len(), 2);
        let mut v = all();
        assert_eq!(shape(&mut v, None, Some("app"), false), 2);
        let mut v = all();
        assert_eq!(shape(&mut v, Some(0), Some(".log"), true), 2);
        let mut v = all();
        assert_eq!(shape(&mut v, None, None, true), 5);
    }

    #[test]
    fn upload_plan_walks_folders() {
        let dir = std::env::temp_dir().join(format!("sshdeck-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pasta/sub")).unwrap();
        std::fs::write(dir.join("pasta/a.txt"), "12345").unwrap();
        std::fs::write(dir.join("pasta/sub/b.txt"), "xy").unwrap();
        let mut steps = Vec::new();
        plan_upload(&dir.join("pasta"), "/srv", &mut steps, 0).unwrap();
        let got: Vec<_> = steps
            .iter()
            .map(|s| (s.dst.as_str(), s.dir, s.size))
            .collect();
        assert_eq!(
            got,
            vec![
                ("/srv/pasta", true, 0),
                ("/srv/pasta/a.txt", false, 5),
                ("/srv/pasta/sub", true, 0),
                ("/srv/pasta/sub/b.txt", false, 2),
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
