//! sshdeck gráfico: a janela é React (gui/src), e aqui ficam os comandos que ela chama.
//! Cofre, ssh config e o resto vêm do `sshdeck-core`.

mod clip;
mod files;
mod pty;
mod rdpclip;
mod rdpview;
mod update;

use portable_pty::CommandBuilder;
use pty::Pty;
use serde::Serialize;
use sshdeck_core::{
    askpass,
    complete::{self, Listing, RemoteLister},
    hosts::{self, HostForm},
    lang,
    sshconfig,
    sysinfo::{self, SysInfo},
    transfer::{self, Transfer},
    vault::{self, Kind, Store, Vault},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tauri::{
    State,
    ipc::{Channel, InvokeResponseBody},
};
use zeroize::Zeroizing;

#[derive(Default)]
struct AppState {
    vault: Mutex<Option<(Vault, Store)>>,
    ptys: Mutex<HashMap<u32, Pty>>,
    next_id: AtomicU32,
    clip: Mutex<clip::Clip>,
    /// sessões RDP embutidas (IronRDP), por id da aba
    rdps: Mutex<HashMap<u32, rdpview::RdpView>>,
    rdp_clip_focus: rdpclip::Focus,
    transfers: Mutex<Vec<Transfer>>,
    listers: Mutex<HashMap<u32, RemoteLister>>,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// O que a janela vê de um host: nunca a senha, só se existe uma salva.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostView {
    alias: String,
    kind: &'static str,
    hostname: String,
    user: String,
    port: u16,
    identity_file: String,
    domain: String,
    gateway: String,
    resolution: String,
    notes: String,
    has_password: bool,
    last_used: Option<u64>,
    created: Option<u64>,
    updated: Option<u64>,
    sysinfo: Option<SysInfo>,
    sysinfo_allowed: Option<bool>,
}

fn host_views(store: &Store) -> Vec<HostView> {
    store
        .hosts
        .iter()
        .map(|h| HostView {
            alias: h.alias.clone(),
            kind: match h.kind {
                Kind::Ssh => "ssh",
                Kind::Rdp => "rdp",
            },
            hostname: h.hostname.clone(),
            user: h.user.clone(),
            port: h.port,
            identity_file: h.identity_file.clone(),
            domain: h.domain.clone(),
            gateway: h.gateway.clone(),
            resolution: h.resolution.clone(),
            notes: h.notes.clone(),
            has_password: !h.password.is_empty(),
            last_used: h.last_used,
            created: h.created,
            updated: h.updated,
            sysinfo: h.sysinfo.clone(),
            sysinfo_allowed: h.sysinfo_allowed,
        })
        .collect()
}

/// Senha salva do host (vazia se não tiver).
fn password_of(state: &AppState, alias: &str) -> Res<Zeroizing<String>> {
    let guard = state.vault.lock().unwrap();
    let (_, store) = guard.as_ref().ok_or("cofre bloqueado")?;
    let h = store
        .hosts
        .iter()
        .find(|h| h.alias == alias)
        .ok_or_else(|| format!("host {alias} não existe"))?;
    Ok(Zeroizing::new(h.password.clone()))
}

/// "missing" (criar), "locked" (pedir a senha) ou "unlocked".
#[tauri::command]
fn vault_state(state: State<AppState>) -> &'static str {
    if state.vault.lock().unwrap().is_some() {
        "unlocked"
    } else if Vault::default_path().exists() {
        "locked"
    } else {
        "missing"
    }
}

#[tauri::command]
fn vault_unlock(state: State<AppState>, password: String) -> Res<()> {
    let (vault, store) = Vault::open(Vault::default_path(), &password).map_err(err)?;
    if lang::forced().is_none()
        && let Some(code) = store.lang.as_deref().and_then(lang::parse)
    {
        lang::set(code);
    }
    // garante que o ssh config reflete o cofre, como a TUI faz
    sshconfig::write(&store.hosts).map_err(err)?;
    *state.vault.lock().unwrap() = Some((vault, store));
    Ok(())
}

#[tauri::command]
fn vault_create(state: State<AppState>, password: String) -> Res<()> {
    if password.chars().count() < 4 {
        return Err(lang::tr("pw.min_len"));
    }
    let vault = Vault::create(Vault::default_path(), &password).map_err(err)?;
    *state.vault.lock().unwrap() = Some((vault, Store::default()));
    Ok(())
}

#[tauri::command]
fn hosts(state: State<AppState>) -> Res<Vec<HostView>> {
    let guard = state.vault.lock().unwrap();
    let (_, store) = guard.as_ref().ok_or("cofre bloqueado")?;
    Ok(host_views(store))
}

/// Guarda a hora da conexão, como a TUI. Devolve a lista atualizada.
#[tauri::command]
fn mark_used(state: State<AppState>, alias: String) -> Res<Vec<HostView>> {
    let mut guard = state.vault.lock().unwrap();
    let (vault, store) = guard.as_mut().ok_or("cofre bloqueado")?;
    if let Some(h) = store.hosts.iter_mut().find(|h| h.alias == alias) {
        h.last_used = Some(vault::now());
    }
    vault.save(store).map_err(err)?;
    sshconfig::write(&store.hosts).map_err(err)?;
    Ok(host_views(store))
}

/// Copia a senha salva (limpa sozinha depois). Devolve a mensagem de status.
#[tauri::command]
fn copy_password(state: State<AppState>, alias: String) -> Res<String> {
    let pw = password_of(&state, &alias)?;
    if pw.is_empty() {
        return Ok(tr_args("st.no_password", &[("alias", &alias)]));
    }
    let how = state.clip.lock().unwrap().copy_secret(&pw).map_err(err)?;
    Ok(tr_args(
        "st.pw_copied",
        &[
            ("alias", &alias),
            ("how", how),
            ("secs", &clip::CLEAR_AFTER.as_secs().to_string()),
        ],
    ))
}

/// Alt+P: digita a senha salva no terminal da sessão.
#[tauri::command]
fn type_password(state: State<AppState>, id: u32, alias: String) -> Res<String> {
    let pw = password_of(&state, &alias)?;
    if pw.is_empty() {
        return Ok(tr_args("st.no_password", &[("alias", &alias)]));
    }
    if let Some(p) = state.ptys.lock().unwrap().get_mut(&id) {
        p.write(pw.as_bytes());
    }
    Ok(tr_args("st.pw_typed", &[("alias", &alias)]))
}

/// Roda `f` com o cofre aberto, salva e regrava o ssh config. Devolve a lista atualizada.
fn with_store<T>(
    state: &AppState,
    f: impl FnOnce(&mut Store) -> anyhow::Result<T>,
) -> Res<(T, Vec<HostView>)> {
    let mut guard = state.vault.lock().unwrap();
    let (vault, store) = guard.as_mut().ok_or("cofre bloqueado")?;
    let out = f(store).map_err(err)?;
    vault.save(store).map_err(err)?;
    sshconfig::write(&store.hosts).map_err(err)?;
    Ok((out, host_views(store)))
}

fn host_clone(state: &AppState, alias: &str) -> Res<vault::Host> {
    let guard = state.vault.lock().unwrap();
    let (_, store) = guard.as_ref().ok_or("cofre bloqueado")?;
    store
        .hosts
        .iter()
        .find(|h| h.alias == alias)
        .cloned()
        .ok_or_else(|| format!("host {alias} não existe"))
}

/// Formulário de edição (com a senha, como o formulário da TUI).
#[tauri::command]
fn host_get(state: State<AppState>, alias: String) -> Res<HostForm> {
    Ok(HostForm::from_host(&host_clone(&state, &alias)?))
}

/// Texto cinza do IdentityFile vazio: qual chave o ssh vai usar.
#[tauri::command]
fn key_hint() -> String {
    sshconfig::default_key_hint()
}

/// Salva um host novo (`editing = None`) ou a edição do host `editing`.
#[tauri::command]
fn host_save(
    state: State<AppState>,
    editing: Option<String>,
    form: HostForm,
) -> Res<Vec<HostView>> {
    let host = form.to_host().map_err(err)?;
    let (_, views) = with_store(&state, |store| {
        let idx = match &editing {
            Some(a) => Some(
                store
                    .hosts
                    .iter()
                    .position(|h| &h.alias == a)
                    .ok_or_else(|| anyhow::anyhow!("host {a} não existe"))?,
            ),
            None => None,
        };
        hosts::upsert(store, idx, host)
    })?;
    Ok(views)
}

#[tauri::command]
fn host_delete(state: State<AppState>, alias: String) -> Res<Vec<HostView>> {
    Ok(with_store(&state, |store| {
        store.hosts.retain(|h| h.alias != alias);
        Ok(())
    })?
    .1)
}

#[derive(Serialize)]
struct Imported {
    added: usize,
    hosts: Vec<HostView>,
}

#[tauri::command]
fn hosts_import(state: State<AppState>) -> Res<Imported> {
    let (added, hosts) = with_store(&state, hosts::import)?;
    Ok(Imported { added, hosts })
}

/// Resposta ao popup da primeira conexão (coletar as informações do servidor?).
#[tauri::command]
fn sysinfo_allow(state: State<AppState>, alias: String, allowed: bool) -> Res<Vec<HostView>> {
    Ok(with_store(&state, |store| {
        if let Some(h) = store.hosts.iter_mut().find(|h| h.alias == alias) {
            h.sysinfo_allowed = Some(allowed);
        }
        Ok(())
    })?
    .1)
}

/// Coleta sistema/kernel/hardware por uma segunda conexão ssh (demora alguns segundos).
#[tauri::command(async)]
fn sysinfo_collect(state: State<'_, AppState>, alias: String) -> Res<Vec<HostView>> {
    let host = host_clone(&state, &alias)?;
    if host.kind != Kind::Ssh {
        return Err(lang::tr("st.sysinfo_ssh_only"));
    }
    let info = sysinfo::collect(&alias, &host.password).map_err(|e| {
        let e = format!("{e:#}");
        if e.contains("Host key verification failed") {
            tr_args("st.sysinfo_unknown_key", &[("alias", &alias)])
        } else {
            tr_args("st.sysinfo_failed", &[("alias", &alias), ("err", &e)])
        }
    })?;
    Ok(with_store(&state, |store| {
        if let Some(h) = store.hosts.iter_mut().find(|h| h.alias == alias) {
            h.sysinfo_allowed = Some(true);
            h.sysinfo = Some(info);
        }
        Ok(())
    })?
    .1)
}

/// Troca a senha mestra (o cofre é recriptografado com um salt novo).
#[tauri::command]
fn vault_rekey(state: State<AppState>, password: String) -> Res<String> {
    if password.chars().count() < 4 {
        return Err(lang::tr("pw.min_len"));
    }
    let mut guard = state.vault.lock().unwrap();
    let (vault, store) = guard.as_mut().ok_or("cofre bloqueado")?;
    vault.rekey(&password, store).map_err(err)?;
    Ok(lang::tr("st.master_changed"))
}

#[derive(Serialize)]
struct LangView {
    code: &'static str,
    name: &'static str,
}

#[tauri::command]
fn langs() -> (Vec<LangView>, String) {
    let list = lang::LANGS
        .iter()
        .zip(lang::NAMES)
        .map(|(code, name)| LangView { code, name })
        .collect();
    (list, lang::current())
}

/// Troca o idioma e guarda a escolha no cofre (vale também para a TUI).
#[tauri::command]
fn set_lang(state: State<AppState>, code: String) -> Res<String> {
    let code = lang::parse(&code).ok_or("idioma desconhecido")?;
    lang::set(code);
    with_store(&state, |store| {
        store.lang = Some(code.to_string());
        Ok(())
    })?;
    Ok(lang::tr("st.lang"))
}

// --- RDP (embutido, com IronRDP) ---

/// Abre o RDP numa aba: a tela chega em `on_frame` e o estado em `on_event`.
#[tauri::command]
fn rdp_open(
    state: State<AppState>,
    alias: String,
    width: u16,
    height: u16,
    on_frame: Channel<InvokeResponseBody>,
    on_event: Channel<rdpview::Event>,
) -> Res<u32> {
    let host = host_clone(&state, &alias)?;
    if host.kind != Kind::Rdp {
        return Err(format!("{alias} não é RDP"));
    }
    let gateway = if host.gateway.is_empty() {
        None
    } else {
        Some(
            host_clone(&state, &host.gateway)
                .map_err(|_| tr_args("st.gateway_missing", &[("gw", &host.gateway)]))?,
        )
    };
    let known = host.rdp_cert.clone();
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let view = rdpview::RdpView::start(
        host, gateway, width, height, known, on_frame, on_event, id, state.rdp_clip_focus.clone(),
    );
    state.rdps.lock().unwrap().insert(id, view);
    drop(mark_used(state, alias));
    Ok(id)
}

/// Mouse, teclado e redimensionamento vindos do canvas.
#[derive(serde::Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum RdpInput {
    Move { x: u16, y: u16 },
    Button { x: u16, y: u16, b: u8, down: bool },
    Wheel { x: u16, y: u16, d: i16, h: bool },
    Key { c: u16, down: bool },
    Sync { caps: bool, num: bool, scroll: bool },
    Resize { w: u16, h: u16 },
}

#[tauri::command]
fn rdp_input(state: State<AppState>, id: u32, input: RdpInput) {
    use rdpview::Input;
    let input = match input {
        RdpInput::Move { x, y } => Input::Mouse { x, y },
        RdpInput::Button { x, y, b, down } => Input::Button { x, y, button: b, down },
        RdpInput::Wheel { x, y, d, h } => Input::Wheel { x, y, delta: d, horizontal: h },
        RdpInput::Key { c, down } => Input::Key { code: c, down },
        RdpInput::Sync { caps, num, scroll } => Input::Sync { caps, num, scroll },
        RdpInput::Resize { w, h } => Input::Resize { width: w, height: h },
    };
    if let Some(v) = state.rdps.lock().unwrap().get(&id) {
        v.send(input);
    }
}

/// Informa a única aba que pode usar o clipboard; `None` suspende todas.
/// Não espera pela área de transferência na thread da janela (ela pode ser dona da seleção X11).
#[tauri::command]
async fn rdp_clipboard_focus(state: State<'_, AppState>, id: Option<u32>, revision: u64) -> Res<()> {
    if state.rdp_clip_focus.update(id, revision)
        && let Some(id) = id
    {
        let found = state.rdps.lock().unwrap().get(&id).map(|view| {
            view.send(rdpview::Input::ClipboardSync);
        });
        if found.is_none() {
            state.rdp_clip_focus.close(id);
        }
    }
    Ok(())
}

/// Alt+P numa aba RDP: digita a senha salva como texto.
#[tauri::command]
fn rdp_type_password(state: State<AppState>, id: u32, alias: String) -> Res<String> {
    let pw = password_of(&state, &alias)?;
    if pw.is_empty() {
        return Ok(tr_args("st.no_password", &[("alias", &alias)]));
    }
    if let Some(v) = state.rdps.lock().unwrap().get(&id) {
        v.send(rdpview::Input::Text(pw));
    }
    Ok(tr_args("st.pw_typed", &[("alias", &alias)]))
}

#[tauri::command]
async fn rdp_close(state: State<'_, AppState>, id: u32) -> Res<()> {
    let view = state.rdps.lock().unwrap().remove(&id);
    state.rdp_clip_focus.close(id);
    // Drop encerra a sessão (e o túnel, se houver)
    drop(view);
    Ok(())
}

/// Guarda a impressão digital do certificado aceito (1ª conexão ou "confiar no novo").
#[tauri::command]
fn rdp_cert_save(state: State<AppState>, alias: String, fingerprint: String) -> Res<Vec<HostView>> {
    Ok(with_store(&state, |store| {
        if let Some(h) = store.hosts.iter_mut().find(|h| h.alias == alias) {
            h.rdp_cert = fingerprint;
        }
        Ok(())
    })?
    .1)
}

// --- rsync ---

#[tauri::command]
fn transfer_defaults() -> String {
    transfer::Request::to_downloads().local
}

#[tauri::command]
fn transfer_start(
    state: State<AppState>,
    alias: String,
    upload: bool,
    remote: String,
    local: String,
) -> Res<String> {
    let host = host_clone(&state, &alias)?;
    if host.kind != Kind::Ssh {
        return Err(lang::tr("st.ssh_only"));
    }
    let mut transfers = state.transfers.lock().unwrap();
    if transfers.iter().any(|t| t.alias == alias) {
        return Err(tr_args("st.transfer_busy", &[("alias", &alias)]));
    }
    let req = transfer::Request {
        upload,
        remote: remote.trim().to_string(),
        local: local.trim().to_string(),
    };
    transfers.push(Transfer::start(&host, &req).map_err(err)?);
    Ok(tr_args("gui.transfer_started", &[("alias", &alias)]))
}

#[tauri::command]
fn transfer_cancel(state: State<AppState>, alias: String) {
    state.transfers.lock().unwrap().retain(|t| t.alias != alias);
}

/// Abre a conexão escondida que lista as pastas do servidor para o autocomplete.
#[tauri::command(async)]
fn ls_open(state: State<'_, AppState>, alias: String) -> Res<u32> {
    let host = host_clone(&state, &alias)?;
    let mut lister =
        RemoteLister::start(&host, Arc::new(AtomicBool::new(false))).map_err(err)?;
    lister.request("");
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    state.listers.lock().unwrap().insert(id, lister);
    Ok(id)
}

#[tauri::command]
fn ls_close(state: State<AppState>, id: u32) {
    state.listers.lock().unwrap().remove(&id);
}

#[derive(Serialize)]
struct Suggest {
    /// "loading", "failed" ou "ready"
    state: &'static str,
    error: String,
    items: Vec<String>,
}

/// Sugestões para o caminho digitado: no servidor (`lister`) ou no disco local.
#[tauri::command]
fn ls_suggest(state: State<AppState>, lister: Option<u32>, input: String) -> Suggest {
    let (dir, prefix) = complete::split(&input);
    let listing = match lister {
        Some(id) => match state.listers.lock().unwrap().get_mut(&id) {
            Some(ls) => {
                let dir = complete::remote_dir(dir);
                ls.request(dir);
                ls.get(dir)
            }
            None => Listing::Failed("ssh".into()),
        },
        None => complete::local(dir),
    };
    match listing {
        Listing::Loading => Suggest { state: "loading", error: String::new(), items: vec![] },
        Listing::Failed(e) => Suggest { state: "failed", error: e, items: vec![] },
        Listing::Ready(entries) => {
            let names = complete::matches(&entries, prefix);
            // já digitado por inteiro: não sobra nada para sugerir
            let items = if names == [prefix] {
                vec![]
            } else {
                names.into_iter().map(String::from).collect()
            };
            Suggest { state: "ready", error: String::new(), items }
        }
    }
}

// --- acompanhamento: RDP e rsync em segundo plano ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferView {
    alias: String,
    upload: bool,
    percent: u8,
    bytes: String,
    rate: String,
    eta: String,
}

#[derive(Serialize)]
struct Event {
    msg: String,
}

#[derive(Serialize)]
struct Tick {
    transfers: Vec<TransferView>,
    events: Vec<Event>,
}

fn elapsed(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Chamado periodicamente pela janela: estado dos rsync em segundo plano.
#[tauri::command]
fn tick(state: State<AppState>) -> Tick {
    let mut events = Vec::new();
    let mut transfers = state.transfers.lock().unwrap();
    let mut i = 0;
    while i < transfers.len() {
        let Some(result) = transfers[i].poll() else {
            i += 1;
            continue;
        };
        let t = transfers.remove(i);
        let time = elapsed(t.started.elapsed().as_secs());
        let msg = match result {
            Ok(bytes) => tr_args(
                "st.transfer_ok",
                &[("alias", &t.alias), ("bytes", &bytes), ("time", &time)],
            ),
            Err(e) => tr_args("st.transfer_failed", &[("alias", &t.alias), ("err", &e)]),
        };
        events.push(Event { msg });
    }
    let transfer_views = transfers
        .iter()
        .map(|t| {
            let p = t.progress();
            TransferView {
                alias: t.alias.clone(),
                upload: t.upload,
                percent: p.percent,
                bytes: p.bytes,
                rate: p.rate,
                eta: p.eta,
            }
        })
        .collect();
    Tick {
        transfers: transfer_views,
        events,
    }
}

fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    args.iter().fold(lang::tr(key), |text, (k, v)| {
        text.replace(&format!("%{{{k}}}"), v)
    })
}

/// Traduções das chaves pedidas, no idioma atual.
#[tauri::command]
fn translations(keys: Vec<String>) -> HashMap<String, String> {
    keys.into_iter()
        .map(|k| {
            let v = lang::tr(&k);
            (k, v)
        })
        .collect()
}

#[tauri::command]
fn ssh_open(
    state: State<AppState>,
    alias: String,
    rows: u16,
    cols: u16,
    on_data: Channel<InvokeResponseBody>,
    on_exit: Channel<()>,
) -> Res<u32> {
    let pty = Pty::ssh(&alias, rows, cols, on_data, on_exit).map_err(err)?;
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    state.ptys.lock().unwrap().insert(id, pty);
    Ok(id)
}

/// `ssh-copy-id` numa aba de terminal: a senha salva é preenchida pelo askpass e as outras
/// perguntas (fingerprint novo, passphrase) aparecem no próprio terminal.
#[tauri::command]
fn copy_id_open(
    state: State<AppState>,
    alias: String,
    rows: u16,
    cols: u16,
    on_data: Channel<InvokeResponseBody>,
    on_exit: Channel<()>,
) -> Res<u32> {
    let host = host_clone(&state, &alias)?;
    if host.kind != Kind::Ssh {
        return Err(lang::tr("st.ssh_only"));
    }
    // o sh mostra o resultado no fim, já que a aba continua aberta depois
    let mut cmd = CommandBuilder::new("sh");
    cmd.args([
        "-c",
        r#"printf '\033[2m→ ssh-copy-id %s\033[0m\n\n' "$1"; ssh-copy-id "$@" && printf '\n\033[32m%s\033[0m\n' "$SSHDECK_DONE" || printf '\n\033[31m%s\033[0m\n' "$SSHDECK_FAILED""#,
        "sh",
    ]);
    if !host.identity_file.is_empty() {
        cmd.arg("-i");
        cmd.arg(transfer::expand_tilde(&host.identity_file));
    }
    cmd.arg(&alias);
    cmd.env("SSHDECK_DONE", lang::tr("gui.copyid_done"));
    cmd.env("SSHDECK_FAILED", lang::tr("gui.copyid_failed"));
    if !host.password.is_empty() {
        cmd.env("SSH_ASKPASS", std::env::current_exe().map_err(err)?);
        cmd.env("SSH_ASKPASS_REQUIRE", "force");
        cmd.env(askpass::PASSWORD_ENV, &host.password);
    }
    let pty = Pty::spawn(cmd, rows, cols, on_data, on_exit).map_err(err)?;
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    state.ptys.lock().unwrap().insert(id, pty);
    Ok(id)
}

#[tauri::command]
fn ssh_write(state: State<AppState>, id: u32, data: String) {
    if let Some(p) = state.ptys.lock().unwrap().get_mut(&id) {
        p.write(data.as_bytes());
    }
}

#[tauri::command]
fn ssh_resize(state: State<AppState>, id: u32, rows: u16, cols: u16) {
    if let Some(p) = state.ptys.lock().unwrap().get(&id) {
        p.resize(rows, cols);
    }
}

#[tauri::command]
fn ssh_close(state: State<AppState>, id: u32) {
    state.ptys.lock().unwrap().remove(&id);
}

fn main() {
    // chamado pelo ssh como SSH_ASKPASS: responde e sai antes de abrir janela
    if let Some(code) = askpass::run_if_requested() {
        std::process::exit(code);
    }
    lang::set(lang::detect());
    // TLS do atualizador (reqwest) e do RDP: o rustls usa o ring, um só para o processo todo
    let _ = rustls::crypto::ring::default_provider().install_default();

    tauri::Builder::default()
        .manage(AppState::default())
        .manage(update::Updater::new())
        .manage(files::FilesState::default())
        .setup(|app| {
            update::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_state,
            vault_unlock,
            vault_create,
            hosts,
            mark_used,
            copy_password,
            type_password,
            translations,
            ssh_open,
            ssh_write,
            ssh_resize,
            ssh_close,
            copy_id_open,
            host_get,
            key_hint,
            host_save,
            host_delete,
            hosts_import,
            sysinfo_allow,
            sysinfo_collect,
            vault_rekey,
            langs,
            set_lang,
            rdp_open,
            rdp_input,
            rdp_clipboard_focus,
            rdp_type_password,
            rdp_close,
            rdp_cert_save,
            transfer_defaults,
            transfer_start,
            transfer_cancel,
            ls_open,
            ls_close,
            ls_suggest,
            tick,
            files::files_probe,
            files::files_open,
            files::files_close,
            files::files_list,
            files::files_existing,
            files::files_mkdir,
            files::files_rename,
            files::files_remove,
            files::files_copy,
            files::files_cancel,
            update::update_status,
            update::update_check,
            update::update_download,
            update::update_install,
            update::update_open_page,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao iniciar o sshdeck");
}
