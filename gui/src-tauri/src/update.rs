//! Atualizações pelos releases do GitHub (gabrielwsa/sshdeck), no modelo do Oxide:
//! - AppImage: baixa a nova, troca o arquivo e abre de novo (sem senha);
//! - pacote do sistema (.pkg.tar.zst, .deb, .rpm): baixa e instala com `pkexec`
//!   (o polkit pede a senha do sistema) e abre de novo;
//! - o resto (cargo install, binário solto): só avisa e abre a página do release.
//!
//! Todo download é conferido com o SHA256SUMS publicado no mesmo release.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sshdeck_core::lang;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};

const REPO: &str = "gabrielwsa/sshdeck";
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);
const UA: &str = concat!("sshdeck-updater/", env!("CARGO_PKG_VERSION"));

/// Como este sshdeck foi instalado: decide o arquivo do release e como instalar.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Install {
    AppImage,
    Pacman,
    Apt,
    Dnf,
    Zypper,
    Rpm,
    /// cargo install, binário solto, build de desenvolvimento
    Manual,
}

impl Install {
    fn detect(exe: &Path) -> Self {
        if std::env::var_os("APPIMAGE").is_some() {
            return Self::AppImage;
        }
        if !exe.starts_with("/usr") {
            return Self::Manual;
        }
        let owns = |cmd: &str, args: &[&str]| {
            Command::new(cmd)
                .args(args)
                .arg(exe)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        let has = |p: &str| Path::new(p).exists();
        if owns("pacman", &["-Qqo"]) {
            Self::Pacman
        } else if owns("dpkg", &["-S"]) {
            if has("/usr/bin/apt-get") { Self::Apt } else { Self::Manual }
        } else if owns("rpm", &["-qf"]) {
            if has("/usr/bin/dnf") {
                Self::Dnf
            } else if has("/usr/bin/zypper") {
                Self::Zypper
            } else {
                Self::Rpm
            }
        } else {
            Self::Manual
        }
    }

    /// O arquivo do release que serve para este sistema.
    fn matches(self, name: &str) -> bool {
        match self {
            Self::AppImage => name.ends_with(".AppImage"),
            Self::Pacman => name.ends_with(".pkg.tar.zst"),
            Self::Apt => name.ends_with(".deb"),
            Self::Dnf | Self::Zypper | Self::Rpm => name.ends_with(".rpm"),
            Self::Manual => true,
        }
    }

    fn package(self) -> bool {
        !matches!(self, Self::AppImage | Self::Manual)
    }

    /// Comando que instala o pacote baixado (como root).
    fn command(self, file: &Path) -> Vec<String> {
        let f = file.display().to_string();
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).chain([f.clone()]).collect();
        match self {
            Self::Pacman => v(&["pacman", "-U", "--noconfirm"]),
            // apt-get (e não dpkg -i) instala também as dependências novas
            Self::Apt => v(&["apt-get", "install", "-y", "--allow-downgrades"]),
            Self::Dnf => v(&["dnf", "install", "-y"]),
            Self::Zypper => v(&["zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]),
            Self::Rpm => v(&["rpm", "-U"]),
            Self::AppImage | Self::Manual => vec![],
        }
    }
}

/// O que a janela mostra (evento "update-status").
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// idle · checking · none · available · downloading · ready · installing · error
    state: &'static str,
    current: String,
    version: Option<String>,
    url: String,
    /// "auto" = baixa e instala aqui; "manual" = abre a página do release
    mode: &'static str,
    needs_password: bool,
    percent: u8,
    error: Option<String>,
}

pub struct Updater {
    status: Mutex<Status>,
    install: Install,
    /// caminho do executável (ou do AppImage), guardado antes de uma atualização trocá-lo
    exe: PathBuf,
    /// release encontrado e arquivo baixado, pronto para instalar
    release: Mutex<Option<Release>>,
    file: Mutex<Option<PathBuf>>,
}

#[derive(Clone)]
struct Release {
    version: String,
    url: String,
    assets: Vec<Asset>,
}

#[derive(Clone, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

impl Updater {
    pub fn new() -> Self {
        let exe = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_default();
        let mut install = Install::detect(&exe);
        // build de desenvolvimento: nunca se instala por cima (só avisa)
        if cfg!(debug_assertions) && install != Install::AppImage {
            install = Install::Manual;
        }
        let root = geteuid() == 0;
        Self {
            status: Mutex::new(Status {
                state: "idle",
                current: current_version(),
                version: None,
                url: releases_page(),
                mode: if install == Install::Manual { "manual" } else { "auto" },
                needs_password: install.package() && !root,
                percent: 0,
                error: None,
            }),
            install,
            exe,
            release: Mutex::new(None),
            file: Mutex::new(None),
        }
    }

    fn get(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn set(&self, app: &AppHandle, f: impl FnOnce(&mut Status)) {
        let s = {
            let mut s = self.status.lock().unwrap();
            f(&mut s);
            s.clone()
        };
        let _ = app.emit("update-status", s);
    }
}

fn geteuid() -> u32 {
    // sem libc só para isto: /proc diz o uid efetivo
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(str::to_owned))
        })
        .and_then(|u| u.parse().ok())
        .unwrap_or(1000)
}

/// Versão instalada; `SSHDECK_UPDATE_CURRENT` finge outra (para testar o fluxo).
fn current_version() -> String {
    std::env::var("SSHDECK_UPDATE_CURRENT").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into())
}

/// Endereços do GitHub; `SSHDECK_UPDATE_API`/`SSHDECK_UPDATE_SITE` apontam para um servidor de teste.
fn api_base() -> String {
    std::env::var("SSHDECK_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into())
}
fn site_base() -> String {
    std::env::var("SSHDECK_UPDATE_SITE").unwrap_or_else(|_| "https://github.com".into())
}
fn releases_page() -> String {
    format!("{}/{REPO}/releases/latest", site_base())
}

/// Semver: "1.2.10" > "1.2.9", "1.0.0" > "1.0.0-rc.1"; o que vem depois de "+" não conta.
pub fn newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (Vec<u64>, Vec<String>) {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let v = v.split('+').next().unwrap_or("");
        let (core, pre) = v.split_once('-').unwrap_or((v, ""));
        let nums = core.split('.').map(|x| x.parse().unwrap_or(0)).collect();
        let pre = if pre.is_empty() { vec![] } else { pre.split('.').map(str::to_owned).collect() };
        (nums, pre)
    }
    let (na, pa) = parse(a);
    let (nb, pb) = parse(b);
    for i in 0..3 {
        let (x, y) = (na.get(i).copied().unwrap_or(0), nb.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    // a versão final é mais nova que os pré-lançamentos dela
    if pa.is_empty() || pb.is_empty() {
        return pa.is_empty() && !pb.is_empty();
    }
    for i in 0..pa.len().max(pb.len()) {
        let (Some(x), Some(y)) = (pa.get(i), pb.get(i)) else {
            return pa.len() > pb.len();
        };
        if x == y {
            continue;
        }
        return match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x > y,
            // identificadores numéricos vêm antes dos alfanuméricos
            (Ok(_), Err(_)) => false,
            (Err(_), Ok(_)) => true,
            _ => x > y,
        };
    }
    false
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// Último release publicado (sem rascunhos nem pré-lançamentos) com o arquivo deste sistema.
async fn latest(install: Install) -> Result<Release, String> {
    let http = client()?;
    let res = http
        .get(format!("{}/repos/{REPO}/releases?per_page=20", api_base()))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    // a API permite 60 consultas por hora por IP: redes compartilhadas esgotam
    if status == 403 || status == 429 {
        return latest_from_site(&http, install)
            .await
            .map_err(|_| lang::tr("upd.rate_limited"));
    }
    if !res.status().is_success() {
        return Err(format!("GitHub HTTP {status}"));
    }
    #[derive(Deserialize)]
    struct ApiRelease {
        tag_name: String,
        html_url: String,
        draft: bool,
        prerelease: bool,
        #[serde(default)]
        assets: Vec<Asset>,
    }
    let list: Vec<ApiRelease> = res.json().await.map_err(|e| e.to_string())?;
    let r = list
        .into_iter()
        .find(|r| !r.draft && !r.prerelease && r.assets.iter().any(|a| install.matches(&a.name)))
        .ok_or_else(|| lang::tr("upd.no_build"))?;
    Ok(Release {
        version: r.tag_name.trim_start_matches('v').to_owned(),
        url: r.html_url,
        assets: r.assets,
    })
}

/// Sem a API: /releases/latest redireciona para a tag, e os arquivos têm nomes previsíveis.
async fn latest_from_site(http: &reqwest::Client, install: Install) -> Result<Release, String> {
    let page = http.get(releases_page()).send().await.map_err(|e| e.to_string())?;
    let url = page.url().to_string();
    let tag = url
        .split("/releases/tag/")
        .nth(1)
        .map(|t| t.split(['?', '#']).next().unwrap_or(t).to_owned())
        .ok_or("tag")?;
    let version = tag.trim_start_matches('v').to_owned();
    let base = format!("{}/{REPO}/releases/download/{tag}/", site_base());
    let assets: Vec<Asset> = asset_names(&version)
        .into_iter()
        .map(|name| Asset {
            browser_download_url: format!("{base}{name}"),
            name,
            size: 0,
        })
        .collect();
    Ok(Release { version, url, assets: assets.into_iter().filter(|a| install.matches(&a.name) || a.name == "SHA256SUMS").collect() })
}

/// Nomes dos arquivos de uma versão, como o workflow de release (.github/workflows/release.yml) os gera.
fn asset_names(v: &str) -> Vec<String> {
    vec![
        format!("sshdeck_{v}_amd64.AppImage"),
        format!("sshdeck_{v}_amd64.deb"),
        format!("sshdeck-{v}-1.x86_64.rpm"),
        format!("sshdeck-{v}-1-x86_64.pkg.tar.zst"),
        "SHA256SUMS".into(),
    ]
}

/// Busca e avisa a janela. Sem erro na tela para buscas automáticas que falham (sem rede…).
pub async fn check(app: &AppHandle, quiet: bool) -> Status {
    let up = app.state::<Updater>();
    let s = up.get();
    if matches!(s.state, "checking" | "downloading" | "installing") {
        return s;
    }
    // um download pronto espera o reinício, a não ser que saia uma versão ainda mais nova
    if s.state == "ready" {
        match latest(up.install).await {
            Ok(r) if newer(&r.version, s.version.as_deref().unwrap_or(&s.current)) => {}
            _ => return up.get(),
        }
        *up.file.lock().unwrap() = None;
    }
    up.set(app, |s| {
        s.state = "checking";
        s.error = None;
    });
    match latest(up.install).await {
        Ok(r) => {
            let is_newer = newer(&r.version, &up.get().current);
            let (version, url) = (r.version.clone(), r.url.clone());
            *up.release.lock().unwrap() = Some(r);
            up.set(app, |s| {
                if is_newer {
                    s.state = "available";
                    s.version = Some(version);
                    s.url = url;
                } else {
                    s.state = "none";
                }
            });
        }
        Err(e) => up.set(app, |s| {
            s.state = if quiet { "idle" } else { "error" };
            s.error = Some(e);
        }),
    }
    up.get()
}

/// Busca ao abrir e depois a cada 4 horas.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            check(&app, true).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// SHA-256 esperado do arquivo, pelo SHA256SUMS do release ("hash  nome").
fn expected_sha(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        (file.trim_start_matches('*') == name).then(|| hash.to_lowercase())
    })
}

async fn download(app: &AppHandle) -> Result<PathBuf, String> {
    let up = app.state::<Updater>();
    let r = up.release.lock().unwrap().clone().ok_or("release")?;
    let asset = r
        .assets
        .iter()
        .find(|a| up.install.matches(&a.name) && a.name != "SHA256SUMS")
        .ok_or_else(|| lang::tr("upd.no_build"))?
        .clone();
    let http = client()?;
    let sums_url = r
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .map(|a| a.browser_download_url.clone())
        .ok_or_else(|| lang::tr("upd.no_sums"))?;
    let sums = http
        .get(sums_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let want = expected_sha(&sums, &asset.name).ok_or_else(|| lang::tr("upd.no_sums"))?;

    // AppImage: baixa ao lado do atual (a troca é um rename); pacote: no cache
    let dir = if up.install == Install::AppImage {
        up.exe.parent().map(Path::to_path_buf).ok_or("AppImage")?
    } else {
        let d = dirs_cache().join("update");
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        d
    };
    let file = if up.install == Install::AppImage {
        dir.join(".sshdeck-update.AppImage")
    } else {
        dir.join(&asset.name)
    };
    let mut res = http
        .get(&asset.browser_download_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| lang::tr("upd.download_failed").replace("%{err}", &e.to_string()))?;
    let total = res.content_length().unwrap_or(asset.size);
    let mut out = std::fs::File::create(&file).map_err(|e| {
        if up.install == Install::AppImage && e.kind() == std::io::ErrorKind::PermissionDenied {
            lang::tr("upd.not_writable").replace("%{dir}", &dir.display().to_string())
        } else {
            e.to_string()
        }
    })?;
    let mut hash = Sha256::new();
    let mut got = 0u64;
    let mut last = 0u8;
    while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
        use std::io::Write;
        hash.update(&chunk);
        out.write_all(&chunk).map_err(|e| e.to_string())?;
        got += chunk.len() as u64;
        let pct = (got * 99).checked_div(total).map_or(0, |p| p.min(99) as u8);
        if pct != last {
            last = pct;
            up.set(app, |s| s.percent = pct);
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    let got_sha: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got_sha != want {
        let _ = std::fs::remove_file(&file);
        return Err(lang::tr("upd.sha_mismatch"));
    }
    Ok(file)
}

fn dirs_cache() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("sshdeck")
}

fn open_url(url: &str) {
    let _ = Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Abre a nova versão (o mesmo caminho de antes) e fecha esta.
fn relaunch(app: &AppHandle, exe: &Path) {
    let _ = Command::new(exe).spawn();
    app.exit(0);
}

// --- comandos da janela ---

#[tauri::command]
pub fn update_status(up: State<Updater>) -> Status {
    up.get()
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> Status {
    check(&app, false).await
}

/// Baixa (modo automático) ou abre a página do release (manual).
#[tauri::command]
pub async fn update_download(app: AppHandle) {
    let up = app.state::<Updater>();
    let s = up.get();
    if s.mode == "manual" {
        open_url(&s.url);
        return;
    }
    if s.state != "available" {
        return;
    }
    up.set(&app, |s| {
        s.state = "downloading";
        s.percent = 0;
        s.error = None;
    });
    match download(&app).await {
        Ok(file) => {
            *up.file.lock().unwrap() = Some(file);
            up.set(&app, |s| {
                s.state = "ready";
                s.percent = 100;
            });
        }
        // o download que falhou deixa a atualização disponível, com o motivo
        Err(e) => up.set(&app, |s| {
            s.state = "available";
            s.percent = 0;
            s.error = Some(e);
        }),
    }
}

/// Instala o que foi baixado e abre a nova versão.
#[tauri::command]
pub async fn update_install(app: AppHandle) {
    let up = app.state::<Updater>();
    if up.get().state != "ready" {
        return;
    }
    let Some(file) = up.file.lock().unwrap().clone().filter(|f| f.exists()) else {
        up.set(&app, |s| s.state = "available");
        return;
    };
    up.set(&app, |s| {
        s.state = "installing";
        s.error = None;
    });

    if up.install == Install::AppImage {
        use std::os::unix::fs::PermissionsExt;
        let done = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
            .and_then(|()| std::fs::rename(&file, &up.exe));
        match done {
            Ok(()) => relaunch(&app, &up.exe),
            Err(e) => up.set(&app, |s| {
                s.state = "ready";
                s.error = Some(e.to_string());
            }),
        }
        return;
    }

    let args = up.install.command(&file);
    let root = geteuid() == 0;
    let mut cmd = if root {
        let mut c = Command::new(&args[0]);
        c.args(&args[1..]);
        c
    } else {
        let mut c = Command::new("pkexec");
        c.args(&args);
        c
    };
    let install = up.install;
    let result = tauri::async_runtime::spawn_blocking(move || cmd.output()).await;
    let error = match result {
        Ok(Ok(out)) if out.status.success() => {
            let _ = std::fs::remove_file(&file);
            return relaunch(&app, &up.exe);
        }
        Ok(Ok(out)) => match out.status.code() {
            // pkexec: 126 = janela da senha fechada, 127 = sem autorização
            Some(126) => lang::tr("upd.cancelled"),
            Some(127) => lang::tr("upd.denied"),
            code => {
                let text = String::from_utf8_lossy(&out.stderr);
                let tail: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                let tail = tail[tail.len().saturating_sub(2)..].join(" ");
                if tail.is_empty() {
                    format!("{install:?}: {}", code.unwrap_or(-1))
                } else {
                    tail
                }
            }
        },
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => lang::tr("upd.no_pkexec"),
        Ok(Err(e)) => e.to_string(),
        Err(e) => e.to_string(),
    };
    up.set(&app, |s| {
        s.state = "ready";
        s.error = Some(error);
    });
}

/// Página do release (notas da versão).
#[tauri::command]
pub fn update_open_page(up: State<Updater>) {
    open_url(&up.get().url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("1.2.10", "1.2.9"));
        assert!(newer("v1.0.0", "1.0.0-rc.1"));
        assert!(newer("1.0.0-rc.10", "1.0.0-rc.9"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0+abc", "0.1.0"));
    }

    #[test]
    fn sums() {
        let s = "abc123  sshdeck_0.2.0_amd64.deb\nDEF456 *sshdeck_0.2.0_amd64.AppImage\n";
        assert_eq!(expected_sha(s, "sshdeck_0.2.0_amd64.deb").as_deref(), Some("abc123"));
        assert_eq!(expected_sha(s, "sshdeck_0.2.0_amd64.AppImage").as_deref(), Some("def456"));
        assert_eq!(expected_sha(s, "outro"), None);
    }

    #[test]
    fn assets_by_install() {
        let names = asset_names("1.0.0");
        let pick = |i: Install| names.iter().find(|n| i.matches(n) && *n != "SHA256SUMS").cloned();
        assert_eq!(pick(Install::Pacman).unwrap(), "sshdeck-1.0.0-1-x86_64.pkg.tar.zst");
        assert_eq!(pick(Install::Apt).unwrap(), "sshdeck_1.0.0_amd64.deb");
        assert_eq!(pick(Install::Dnf).unwrap(), "sshdeck-1.0.0-1.x86_64.rpm");
        assert_eq!(pick(Install::AppImage).unwrap(), "sshdeck_1.0.0_amd64.AppImage");
    }
}
