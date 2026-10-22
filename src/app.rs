use crate::{
    clipboard::Clip,
    lang,
    rdp::{self, RdpSession},
    session::Session,
    sshconfig,
    sysinfo::{self, SysInfo},
    vault::{self, Host, Kind, Store, Vault},
};
use anyhow::{Result, bail};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
    widgets::TableState,
};
use rust_i18n::t;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq)]
pub enum PwPurpose {
    Unlock,
    Create,
    Rekey,
}

type SysinfoSlot = Arc<Mutex<Option<Result<SysInfo, String>>>>;

pub enum Mode {
    Password {
        purpose: PwPurpose,
        input: Zeroizing<String>,
        first: Option<Zeroizing<String>>,
        error: Option<String>,
    },
    List,
    Filter,
    Form(Box<Form>),
    ConfirmDelete(usize),
    ConfirmQuit,
    /// popup da primeira conexão: coletar as informações do servidor?
    ConfirmSysinfo(String),
    /// o certificado do servidor RDP mudou: confiar no novo e reconectar?
    ConfirmCert {
        alias: String,
        change: rdp::CertChange,
    },
    Help,
    /// Popup de idioma; o índice é a linha selecionada em `lang::LANGS`.
    Lang(usize),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Field {
    Kind,
    Alias,
    HostName,
    User,
    Domain,
    Port,
    IdentityFile,
    Resolution,
    Share,
    Gateway,
    Password,
    Notes,
}

impl Field {
    pub fn label(self) -> Cow<'static, str> {
        match self {
            Field::Kind => t!("field.kind"),
            Field::Alias => t!("field.alias"),
            Field::HostName => t!("field.hostname"),
            Field::User => t!("field.user"),
            Field::Domain => t!("field.domain"),
            Field::Port => t!("field.port"),
            Field::IdentityFile => t!("field.identity"),
            Field::Resolution => t!("field.resolution"),
            Field::Share => t!("field.share"),
            Field::Gateway => t!("field.gateway"),
            Field::Password => t!("field.password"),
            Field::Notes => t!("field.notes"),
        }
    }
}

const SSH_FIELDS: &[Field] = &[
    Field::Kind,
    Field::Alias,
    Field::HostName,
    Field::User,
    Field::Port,
    Field::IdentityFile,
    Field::Password,
    Field::Notes,
];
const RDP_FIELDS: &[Field] = &[
    Field::Kind,
    Field::Alias,
    Field::HostName,
    Field::User,
    Field::Domain,
    Field::Port,
    Field::Resolution,
    Field::Share,
    Field::Gateway,
    Field::Password,
    Field::Notes,
];

pub struct Form {
    pub editing: Option<usize>,
    pub kind: Kind,
    /// valores indexados por `Field as usize`
    values: [String; 12],
    /// índice em `fields()`
    pub focus: usize,
    pub show_pw: bool,
    pub error: Option<String>,
    /// texto cinza do IdentityFile vazio: qual chave será usada
    pub identity_hint: String,
}

impl Form {
    fn new() -> Self {
        Self {
            editing: None,
            kind: Kind::Ssh,
            values: Default::default(),
            focus: 0,
            show_pw: false,
            error: None,
            identity_hint: sshconfig::default_key_hint(),
        }
    }

    fn from_host(idx: usize, h: &Host) -> Self {
        let mut f = Self::new();
        f.editing = Some(idx);
        f.kind = h.kind;
        for (field, v) in [
            (Field::Alias, &h.alias),
            (Field::HostName, &h.hostname),
            (Field::User, &h.user),
            (Field::Domain, &h.domain),
            (Field::IdentityFile, &h.identity_file),
            (Field::Resolution, &h.resolution),
            (Field::Share, &h.share),
            (Field::Gateway, &h.gateway),
            (Field::Password, &h.password),
            (Field::Notes, &h.notes),
        ] {
            *f.value_mut(field) = v.clone();
        }
        *f.value_mut(Field::Port) = h.port.to_string();
        f
    }

    pub fn fields(&self) -> &'static [Field] {
        match self.kind {
            Kind::Ssh => SSH_FIELDS,
            Kind::Rdp => RDP_FIELDS,
        }
    }

    pub fn focused(&self) -> Field {
        self.fields()[self.focus]
    }

    pub fn value(&self, f: Field) -> &str {
        &self.values[f as usize]
    }

    fn value_mut(&mut self, f: Field) -> &mut String {
        &mut self.values[f as usize]
    }

    pub fn default_port(&self) -> u16 {
        match self.kind {
            Kind::Ssh => 22,
            Kind::Rdp => rdp::DEFAULT_PORT,
        }
    }

    fn toggle_kind(&mut self) {
        // a porta padrão acompanha o tipo
        let port = self.value(Field::Port).trim().to_string();
        if port == self.default_port().to_string() {
            self.value_mut(Field::Port).clear();
        }
        self.kind = match self.kind {
            Kind::Ssh => Kind::Rdp,
            Kind::Rdp => Kind::Ssh,
        };
    }

    fn to_host(&self) -> Result<Host> {
        let f = |field: Field| self.value(field).trim().to_string();
        let alias = f(Field::Alias);
        if alias.is_empty() {
            bail!("{}", t!("err.alias_required"));
        }
        if alias.contains(char::is_whitespace) || alias.contains(['*', '?', '!', '#', '/']) {
            bail!("{}", t!("err.alias_chars"));
        }
        let hostname = f(Field::HostName);
        if hostname.is_empty() || hostname.contains(char::is_whitespace) {
            bail!("{}", t!("err.hostname"));
        }
        let port = if f(Field::Port).is_empty() {
            self.default_port()
        } else {
            f(Field::Port)
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("{}", t!("err.port")))?
        };
        let rdp = self.kind == Kind::Rdp;
        if rdp && f(Field::User).is_empty() {
            bail!("{}", t!("err.rdp_user"));
        }
        if rdp && !rdp::valid_resolution(&f(Field::Resolution)) {
            bail!("{}", t!("err.resolution"));
        }
        // campos que não pertencem ao tipo não são guardados
        let only = |cond: bool, field: Field| if cond { f(field) } else { String::new() };
        Ok(Host {
            kind: self.kind,
            alias,
            hostname,
            user: f(Field::User),
            port,
            identity_file: only(!rdp, Field::IdentityFile),
            // senha não é trimada: espaços podem fazer parte dela
            password: self.value(Field::Password).to_string(),
            notes: f(Field::Notes),
            domain: only(rdp, Field::Domain),
            resolution: only(rdp, Field::Resolution),
            share: only(rdp, Field::Share),
            gateway: only(rdp, Field::Gateway),
            ..Default::default()
        })
    }
}

/// Comando que roda em tela cheia, fora da TUI.
pub enum External {
    Ssh(String),
    CopyId {
        alias: String,
        identity: String,
        password: Zeroizing<String>,
    },
}

pub struct App {
    pub vault_path: PathBuf,
    pub vault: Option<Vault>,
    pub store: Store,
    pub mode: Mode,
    pub table: TableState,
    pub filter: String,
    pub status: Option<(String, Instant)>,
    pub clip: Clip,
    pub quit: bool,
    /// o loop principal suspende a TUI e roda este comando
    pub pending: Option<External>,
    pub sessions: Vec<Session>,
    pub rdp_sessions: Vec<RdpSession>,
    /// coletas de informações esperando a sessão completar `sysinfo::DELAY`
    sysinfo_pending: Vec<String>,
    sysinfo_running: Vec<(String, SysinfoSlot)>,
    pub active: usize,
    /// teclas vão para o terminal da sessão ativa
    pub term_focus: bool,
    /// tamanho do painel do terminal, calculado pela UI
    pub term_size: (u16, u16),
    /// Onde o painel do terminal foi desenhado, para saber se a roda do mouse está sobre ele.
    pub term_area: Rect,
    pub output: Arc<AtomicBool>,
}

impl App {
    pub fn new() -> Self {
        let vault_path = Vault::default_path();
        let purpose = if vault_path.exists() {
            PwPurpose::Unlock
        } else {
            PwPurpose::Create
        };
        Self {
            vault_path,
            vault: None,
            store: Store::default(),
            mode: Mode::Password {
                purpose,
                input: Default::default(),
                first: None,
                error: None,
            },
            table: TableState::default().with_selected(0),
            filter: String::new(),
            status: None,
            clip: Clip::new(),
            quit: false,
            pending: None,
            sessions: Vec::new(),
            rdp_sessions: Vec::new(),
            sysinfo_pending: Vec::new(),
            sysinfo_running: Vec::new(),
            active: 0,
            term_focus: false,
            term_size: (24, 80),
            term_area: Rect::default(),
            output: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// Depois de abrir uma sessão SSH: na 1ª vez pergunta se pode coletar as informações.
    /// Depois disso, só coleta de novo quando o usuário pede (tecla `U`).
    fn after_connect(&mut self, idx: usize) {
        let h = &self.store.hosts[idx];
        if h.kind == Kind::Ssh && h.sysinfo_allowed.is_none() {
            self.mode = Mode::ConfirmSysinfo(h.alias.clone());
        }
    }

    /// Tecla `U`: coleta agora, sem esperar sessão (a chave do servidor precisa já ser conhecida).
    fn collect_sysinfo(&mut self, idx: usize) {
        if self.store.hosts[idx].kind != Kind::Ssh {
            return self.set_status(t!("st.sysinfo_ssh_only"));
        }
        let alias = self.store.hosts[idx].alias.clone();
        if self.sysinfo_busy(&alias) {
            return;
        }
        self.store.hosts[idx].sysinfo_allowed = Some(true);
        self.start_sysinfo(alias.clone());
        self.set_status(t!("st.sysinfo_started", alias = alias));
    }

    fn start_sysinfo(&mut self, alias: String) {
        let password = self
            .host_by_alias(&alias)
            .map(|h| h.password.clone())
            .unwrap_or_default();
        let slot = Arc::new(Mutex::new(None));
        let (out, a) = (slot.clone(), alias.clone());
        std::thread::spawn(move || {
            let r = sysinfo::collect(&a, &password).map_err(|e| format!("{e:#}"));
            *out.lock().unwrap() = Some(r);
        });
        self.sysinfo_running.push((alias, slot));
    }

    fn key_confirm_cert(&mut self, key: KeyEvent) {
        let Mode::ConfirmCert { alias, change } = std::mem::replace(&mut self.mode, Mode::List)
        else {
            return;
        };
        if !matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y')) {
            return self.set_status(t!("st.cert_kept", alias = alias));
        }
        // sem o certificado antigo, o /cert:tofu aceita e guarda o novo
        if let Err(e) = std::fs::remove_file(&change.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return self.set_status(t!("st.cert_remove_failed", err = e));
        }
        if let Some(idx) = self.store.hosts.iter().position(|h| h.alias == alias) {
            self.open_rdp(idx);
        }
    }

    fn key_confirm_sysinfo(&mut self, key: KeyEvent) {
        let Mode::ConfirmSysinfo(alias) = std::mem::replace(&mut self.mode, Mode::List) else {
            return;
        };
        let yes = matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y'));
        if let Some(h) = self.store.hosts.iter_mut().find(|h| h.alias == alias) {
            h.sysinfo_allowed = Some(yes);
        }
        if yes {
            self.sysinfo_pending.push(alias);
        }
        if let Err(e) = self.persist() {
            self.set_status(t!("st.save_error", err = format!("{e:#}")));
        }
    }

    pub fn sysinfo_busy(&self, alias: &str) -> bool {
        self.sysinfo_pending.iter().any(|a| a == alias)
            || self.sysinfo_running.iter().any(|(a, _)| a == alias)
    }

    fn tick_sysinfo(&mut self) {
        // sessão fechada antes da hora: tenta de novo na próxima conexão
        let sessions = &self.sessions;
        self.sysinfo_pending
            .retain(|a| sessions.iter().any(|s| &s.alias == a && !s.is_exited()));
        let ready: Vec<String> = self
            .sysinfo_pending
            .iter()
            .filter(|a| {
                sessions
                    .iter()
                    .any(|s| &s.alias == *a && s.started.elapsed() >= sysinfo::DELAY)
            })
            .cloned()
            .collect();
        for alias in ready {
            self.sysinfo_pending.retain(|a| *a != alias);
            self.start_sysinfo(alias);
        }

        let mut i = 0;
        while i < self.sysinfo_running.len() {
            let Some(result) = self.sysinfo_running[i].1.lock().unwrap().take() else {
                i += 1;
                continue;
            };
            let (alias, _) = self.sysinfo_running.remove(i);
            match result {
                Ok(info) => {
                    if let Some(h) = self.store.hosts.iter_mut().find(|h| h.alias == alias) {
                        h.sysinfo = Some(info);
                    }
                    self.persist_or_report(&t!("st.sysinfo_ok", alias = alias));
                }
                // servidor nunca conectado: a coleta não pode aceitar a chave sozinha
                Err(e) if e.contains("Host key verification failed") => {
                    self.set_status(t!("st.sysinfo_unknown_key", alias = alias))
                }
                Err(e) => self.set_status(t!("st.sysinfo_failed", alias = alias, err = e)),
            }
        }
    }

    pub fn tick(&mut self) {
        self.tick_sysinfo();
        if let Some((_, t)) = &self.status
            && t.elapsed() > Duration::from_secs(6)
        {
            self.status = None;
        }

        // acompanha as janelas RDP (túnel subindo, janela fechada, erro)
        let mut i = 0;
        while i < self.rdp_sessions.len() {
            match self.rdp_sessions[i].poll() {
                None => i += 1,
                Some(result) => {
                    let s = self.rdp_sessions.remove(i);
                    match result {
                        rdp::End::Closed => self.set_status(t!("st.rdp_closed", alias = s.alias)),
                        rdp::End::Failed(e) => {
                            self.set_status(t!("st.rdp_error", alias = s.alias, err = e))
                        }
                        // não interrompe um formulário ou outro popup: aí só avisa
                        rdp::End::CertChanged(change) if matches!(self.mode, Mode::List) => {
                            self.mode = Mode::ConfirmCert {
                                alias: s.alias.clone(),
                                change,
                            }
                        }
                        rdp::End::CertChanged(change) => self.set_status(t!(
                            "st.cert_changed",
                            alias = s.alias,
                            path = change.path.display()
                        )),
                    }
                }
            }
        }

        if self.sessions.iter().any(|s| s.is_exited()) {
            let active_alias = self.active_session().map(|s| s.alias.clone());
            let mut closed = Vec::new();
            self.sessions.retain(|s| {
                if s.is_exited() {
                    closed.push(s.alias.clone());
                    false
                } else {
                    true
                }
            });
            self.set_status(t!("st.sessions_ended", list = closed.join(", ")));
            self.active = active_alias
                .and_then(|a| self.sessions.iter().position(|s| s.alias == a))
                .unwrap_or(0);
            if self.sessions.is_empty() {
                self.term_focus = false;
            }
        }
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.sessions.get(self.active)
    }

    pub fn host_by_alias(&self, alias: &str) -> Option<&Host> {
        self.store.hosts.iter().find(|h| h.alias == alias)
    }

    /// Estado da conexão do host: `Some(true)` aberta, `Some(false)` RDP esperando o túnel.
    pub fn connection(&self, alias: &str) -> Option<bool> {
        if self.sessions.iter().any(|s| s.alias == alias) {
            return Some(true);
        }
        self.rdp_sessions
            .iter()
            .find(|s| s.alias == alias)
            .map(|s| !s.connecting())
    }

    pub fn has_session(&self, alias: &str) -> bool {
        self.sessions.iter().any(|s| s.alias == alias)
            || self.rdp_sessions.iter().any(|s| s.alias == alias)
    }

    /// Algum RDP esperando o túnel subir: o loop principal checa mais vezes.
    pub fn rdp_connecting(&self) -> bool {
        self.rdp_sessions.iter().any(|s| s.connecting())
    }

    pub fn open_count(&self) -> usize {
        self.sessions.len() + self.rdp_sessions.len()
    }

    fn open_rdp(&mut self, idx: usize) {
        let host = &self.store.hosts[idx];
        if self.rdp_sessions.iter().any(|s| s.alias == host.alias) {
            return self.set_status(t!("st.rdp_already", alias = host.alias));
        }
        let gateway = (!host.gateway.is_empty())
            .then(|| self.host_by_alias(&host.gateway))
            .flatten();
        if !host.gateway.is_empty() && gateway.is_none() {
            return self.set_status(t!("st.gateway_missing", gw = host.gateway));
        }
        match RdpSession::start(host, gateway) {
            Ok(s) => {
                self.set_status(if s.via_tunnel() {
                    t!(
                        "st.rdp_opening_tunnel",
                        gw = host.gateway,
                        alias = host.alias
                    )
                } else {
                    t!("st.rdp_opening", alias = host.alias)
                });
                self.rdp_sessions.push(s);
                self.mark_used(idx);
            }
            Err(e) => self.set_status(t!("st.rdp_failed", err = format!("{e:#}"))),
        }
    }

    fn open_session(&mut self, idx: usize) {
        if self.store.hosts[idx].kind == Kind::Rdp {
            return self.open_rdp(idx);
        }
        let alias = self.store.hosts[idx].alias.clone();
        if let Some(pos) = self.sessions.iter().position(|s| s.alias == alias) {
            self.active = pos;
        } else {
            match Session::spawn(
                &alias,
                self.term_size.0,
                self.term_size.1,
                self.output.clone(),
            ) {
                Ok(s) => {
                    self.sessions.push(s);
                    self.active = self.sessions.len() - 1;
                    self.mark_used(idx);
                    self.after_connect(idx);
                }
                Err(e) => return self.set_status(t!("st.connect_failed", err = format!("{e:#}"))),
            }
        }
        self.term_focus = true;
    }

    /// Ao navegar na lista, mostra no centro a sessão do host selecionado (se houver).
    fn follow_selection(&mut self) {
        if let Some(i) = self.selected_host()
            && let Some(pos) = self
                .sessions
                .iter()
                .position(|s| s.alias == self.store.hosts[i].alias)
        {
            self.active = pos;
        }
    }

    fn select_active_host(&mut self) {
        let Some(alias) = self.active_session().map(|s| s.alias.clone()) else {
            return;
        };
        self.filter.clear();
        if let Some(pos) = self.store.hosts.iter().position(|h| h.alias == alias) {
            self.table.select(Some(pos));
        }
    }

    fn request_quit(&mut self) {
        if self.open_count() == 0 {
            self.quit = true;
        } else {
            self.mode = Mode::ConfirmQuit;
        }
    }

    fn type_password(&mut self) {
        let Some(alias) = self.active_session().map(|s| s.alias.clone()) else {
            return;
        };
        let pw = self
            .host_by_alias(&alias)
            .map(|h| Zeroizing::new(h.password.clone()));
        match pw {
            Some(pw) if !pw.is_empty() => {
                self.sessions[self.active].write(pw.as_bytes());
                self.set_status(t!("st.pw_typed", alias = alias));
            }
            _ => self.set_status(t!("st.no_password", alias = alias)),
        }
    }

    pub fn visible(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        self.store
            .hosts
            .iter()
            .enumerate()
            .filter(|(_, h)| {
                q.is_empty()
                    || h.alias.to_lowercase().contains(&q)
                    || h.hostname.to_lowercase().contains(&q)
                    || h.user.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_host(&self) -> Option<usize> {
        self.visible().get(self.table.selected()?).copied()
    }

    fn clamp_selection(&mut self) {
        let n = self.visible().len();
        let sel = self.table.selected().unwrap_or(0);
        self.table
            .select(if n == 0 { None } else { Some(sel.min(n - 1)) });
    }

    fn persist(&mut self) -> Result<()> {
        self.store.hosts.sort_by_key(|a| a.alias.to_lowercase());
        if let Some(v) = &self.vault {
            v.save(&self.store)?;
        }
        sshconfig::write(&self.store.hosts)
    }

    /// Guarda a hora da conexão. Erro ao salvar só aparece no status: não impede a conexão.
    fn mark_used(&mut self, idx: usize) {
        self.store.hosts[idx].last_used = Some(vault::now());
        if let Err(e) = self.persist() {
            self.set_status(t!("st.save_error", err = format!("{e:#}")));
        }
    }

    fn persist_or_report(&mut self, ok_msg: &str) {
        match self.persist() {
            Ok(()) => self.set_status(ok_msg),
            Err(e) => self.set_status(t!("st.save_error", err = format!("{e:#}"))),
        }
    }

    fn copy_password(&mut self, idx: usize) -> bool {
        let h = &self.store.hosts[idx];
        if h.password.is_empty() {
            self.set_status(t!("st.no_password", alias = h.alias));
            return false;
        }
        let pw = h.password.clone();
        let alias = h.alias.clone();
        let how = self.clip.copy_secret(&pw);
        self.set_status(t!(
            "st.pw_copied",
            alias = alias,
            how = how,
            secs = crate::clipboard::CLEAR_AFTER.as_secs()
        ));
        true
    }


    /// Roda do mouse: sobre o terminal rola a sessão; no resto da tela vale como ↑/↓.
    pub fn on_mouse(&mut self, m: MouseEvent) {
        let up = match m.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return,
        };
        let over_term = matches!(self.mode, Mode::List)
            && !self.sessions.is_empty()
            && self.term_area.contains(Position::new(m.column, m.row));
        if over_term {
            self.sessions[self.active].wheel(up);
            return;
        }
        let key = KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down });
        let focus = std::mem::replace(&mut self.term_focus, false);
        self.on_key(key);
        self.term_focus = focus;
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.term_focus && matches!(self.mode, Mode::List) && !self.sessions.is_empty() {
            return self.key_terminal(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if matches!(self.mode, Mode::ConfirmQuit) {
                self.quit = true;
            } else {
                self.request_quit();
            }
            return;
        }
        match &self.mode {
            Mode::Password { .. } => self.key_password(key),
            Mode::List => self.key_list(key),
            Mode::Filter => self.key_filter(key),
            Mode::Form(_) => self.key_form(key),
            Mode::ConfirmDelete(_) => self.key_confirm(key),
            Mode::ConfirmSysinfo(_) => self.key_confirm_sysinfo(key),
            Mode::ConfirmCert { .. } => self.key_confirm_cert(key),
            Mode::ConfirmQuit => {
                if matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y')) {
                    self.quit = true;
                } else {
                    self.mode = Mode::List;
                }
            }
            Mode::Help => self.mode = Mode::List,
            Mode::Lang(_) => self.key_lang(key),
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        if self.term_focus && matches!(self.mode, Mode::List) {
            if let Some(s) = self.sessions.get_mut(self.active) {
                s.paste(text);
            }
            return;
        }
        let text = text.trim_end_matches(['\n', '\r']);
        match &mut self.mode {
            Mode::Form(f) => {
                let field = f.focused();
                if field != Field::Kind {
                    f.value_mut(field).push_str(text);
                }
            }
            Mode::Password { input, .. } => input.push_str(text),
            Mode::Filter => {
                self.filter.push_str(text);
                self.clamp_selection();
            }
            _ => {}
        }
    }

    fn key_password(&mut self, key: KeyEvent) {
        let Mode::Password {
            purpose,
            input,
            first,
            error,
        } = &mut self.mode
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                if *purpose == PwPurpose::Rekey {
                    self.mode = Mode::List;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            KeyCode::Enter => {
                let purpose = *purpose;
                let typed = std::mem::take(input);
                if purpose == PwPurpose::Unlock {
                    match Vault::open(self.vault_path.clone(), &typed) {
                        Ok((v, store)) => {
                            self.vault = Some(v);
                            self.store = store;
                            self.mode = Mode::List;
                            if lang::forced().is_none()
                                && let Some(code) = self.store.lang.as_deref().and_then(lang::parse)
                            {
                                lang::set(code);
                            }
                            self.clamp_selection();
                            // garante que o ssh config reflete o cofre
                            if let Err(e) = sshconfig::write(&self.store.hosts) {
                                self.set_status(t!("st.sshconfig_error", err = format!("{e:#}")));
                            }
                        }
                        Err(e) => *error = Some(format!("{e:#}")),
                    }
                    return;
                }
                // Create / Rekey: pede duas vezes
                if typed.len() < 4 {
                    *error = Some(t!("pw.min_len").into());
                    return;
                }
                match first.take() {
                    None => {
                        *first = Some(typed);
                        *error = None;
                    }
                    Some(f) if *f != *typed => {
                        *error = Some(t!("pw.mismatch").into());
                    }
                    Some(_) => {
                        let result = if purpose == PwPurpose::Create {
                            Vault::create(self.vault_path.clone(), &typed)
                                .map(|v| self.vault = Some(v))
                        } else {
                            match self.vault.as_mut() {
                                Some(v) => v.rekey(&typed, &self.store),
                                None => Ok(()),
                            }
                        };
                        match result {
                            Ok(()) => {
                                self.mode = Mode::List;
                                self.set_status(if purpose == PwPurpose::Create {
                                    t!("st.vault_created")
                                } else {
                                    t!("st.master_changed")
                                });
                            }
                            Err(e) => {
                                if let Mode::Password { error, .. } = &mut self.mode {
                                    *error = Some(format!("{e:#}"));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn key_terminal(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let n = self.sessions.len();
        let half = (self.term_size.0 / 2).max(1) as isize;
        match key.code {
            // Ctrl+] chega como Ctrl+5 em terminais legados
            KeyCode::Char(']') | KeyCode::Char('5') if ctrl => {
                self.term_focus = false;
                self.select_active_host();
            }
            KeyCode::Char('p') if alt => self.type_password(),
            KeyCode::Left if alt => self.active = (self.active + n - 1) % n,
            KeyCode::Right if alt => self.active = (self.active + 1) % n,
            KeyCode::PageUp if shift => self.sessions[self.active].scroll(half),
            KeyCode::PageDown if shift => self.sessions[self.active].scroll(-half),
            _ => self.sessions[self.active].send_key(key),
        }
    }

    fn key_list(&mut self, key: KeyEvent) {
        let n = self.visible().len();
        let sel = self.table.selected().unwrap_or(0);
        let workspace = !self.sessions.is_empty();
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') if workspace => {
                self.follow_selection();
                self.term_focus = true;
            }
            KeyCode::Char('x') => {
                if let Some(i) = self.selected_host() {
                    let alias = self.store.hosts[i].alias.clone();
                    if let Some(pos) = self.sessions.iter().position(|s| s.alias == alias) {
                        self.sessions.remove(pos);
                        self.active = self.active.min(self.sessions.len().saturating_sub(1));
                        self.set_status(t!("st.session_closed", alias = alias));
                    } else if let Some(pos) =
                        self.rdp_sessions.iter().position(|s| s.alias == alias)
                    {
                        // Drop fecha a janela do FreeRDP e o túnel
                        self.rdp_sessions.remove(pos);
                        self.set_status(t!("st.rdp_closed", alias = alias));
                    }
                }
            }
            KeyCode::Char('s') | KeyCode::Char('K')
                if self
                    .selected_host()
                    .is_some_and(|i| self.store.hosts[i].kind == Kind::Rdp) =>
            {
                self.set_status(t!("st.ssh_only"));
            }
            KeyCode::Char('s') => {
                if let Some(i) = self.selected_host() {
                    self.copy_password(i);
                    self.pending = Some(External::Ssh(self.store.hosts[i].alias.clone()));
                    self.mark_used(i);
                }
            }
            KeyCode::Char('K') => {
                if let Some(i) = self.selected_host() {
                    let h = &self.store.hosts[i];
                    self.pending = Some(External::CopyId {
                        alias: h.alias.clone(),
                        identity: h.identity_file.clone(),
                        password: Zeroizing::new(h.password.clone()),
                    });
                }
            }
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.request_quit();
                } else {
                    self.filter.clear();
                    self.clamp_selection();
                }
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self.table.select(Some((sel + 1) % n)),
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self.table.select(Some((sel + n - 1) % n)),
            KeyCode::Home | KeyCode::Char('g') if n > 0 => self.table.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') if n > 0 => self.table.select(Some(n - 1)),
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('a') => self.mode = Mode::Form(Box::new(Form::new())),
            KeyCode::Char('e') => {
                if let Some(i) = self.selected_host() {
                    self.mode = Mode::Form(Box::new(Form::from_host(i, &self.store.hosts[i])));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(i) = self.selected_host() {
                    self.mode = Mode::ConfirmDelete(i);
                }
            }
            KeyCode::Char('c') | KeyCode::Char('y') => {
                if let Some(i) = self.selected_host() {
                    self.copy_password(i);
                }
            }
            KeyCode::Enter => {
                if let Some(i) = self.selected_host() {
                    self.copy_password(i);
                    self.open_session(i);
                }
            }
            KeyCode::Char('i') => self.import(),
            KeyCode::Char('U') => {
                if let Some(i) = self.selected_host() {
                    self.collect_sysinfo(i);
                }
            }
            KeyCode::Char('L') => {
                let current = lang::current();
                let i = lang::LANGS.iter().position(|l| *l == current).unwrap_or(0);
                self.mode = Mode::Lang(i);
            }
            KeyCode::Char('P') => {
                self.mode = Mode::Password {
                    purpose: PwPurpose::Rekey,
                    input: Default::default(),
                    first: None,
                    error: None,
                }
            }
            _ => {}
        }
        if workspace {
            self.follow_selection();
        }
    }

    fn key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::List;
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Up => {
                self.mode = Mode::List;
                if key.code != KeyCode::Enter {
                    self.key_list(key);
                }
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.table.select(Some(0));
            }
            _ => {}
        }
        self.clamp_selection();
    }

    fn key_form(&mut self, key: KeyEvent) {
        let Mode::Form(f) = &mut self.mode else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let n = f.fields().len();
        let field = f.focused();
        let on_kind = field == Field::Kind;
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                return;
            }
            KeyCode::Char('s') if ctrl => {}
            KeyCode::Char('r') if ctrl => {
                f.show_pw = !f.show_pw;
                return;
            }
            KeyCode::Char('u') if ctrl => {
                f.value_mut(field).clear();
                return;
            }
            KeyCode::Tab | KeyCode::Down => {
                f.focus = (f.focus + 1) % n;
                return;
            }
            KeyCode::BackTab | KeyCode::Up => {
                f.focus = (f.focus + n - 1) % n;
                return;
            }
            // o campo Tipo alterna entre SSH e RDP
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if on_kind => {
                f.toggle_kind();
                return;
            }
            KeyCode::Enter if f.focus < n - 1 => {
                f.focus += 1;
                return;
            }
            KeyCode::Enter => {}
            KeyCode::Backspace if !on_kind => {
                f.value_mut(field).pop();
                return;
            }
            KeyCode::Char(c) if !on_kind && !ctrl => {
                f.value_mut(field).push(c);
                return;
            }
            _ => return,
        }

        // salvar (Ctrl+S ou Enter no último campo)
        let host = match f.to_host() {
            Ok(h) => h,
            Err(e) => {
                f.error = Some(e.to_string());
                return;
            }
        };
        let editing = f.editing;
        let dup = self
            .store
            .hosts
            .iter()
            .enumerate()
            .any(|(i, h)| Some(i) != editing && h.alias == host.alias);
        if dup {
            f.error = Some(t!("err.duplicate", alias = host.alias).into());
            return;
        }
        if !host.gateway.is_empty() {
            let ok = host.gateway != host.alias
                && self
                    .store
                    .hosts
                    .iter()
                    .any(|h| h.alias == host.gateway && h.kind == Kind::Ssh);
            if !ok {
                f.error = Some(t!("err.gateway", gw = host.gateway).into());
                return;
            }
        }
        let alias = host.alias.clone();
        match editing {
            Some(i) => {
                // o formulário só tem os campos editáveis: o resto vem do host antigo
                let old = std::mem::take(&mut self.store.hosts[i]);
                self.store.hosts[i] = Host {
                    last_used: old.last_used,
                    created: old.created,
                    updated: Some(vault::now()),
                    sysinfo_allowed: old.sysinfo_allowed,
                    sysinfo: old.sysinfo,
                    ..host
                };
            }
            None => self.store.hosts.push(Host {
                created: Some(vault::now()),
                ..host
            }),
        }
        self.mode = Mode::List;
        self.persist_or_report(&t!("st.saved", alias = alias));
        self.filter.clear();
        if let Some(pos) = self.store.hosts.iter().position(|h| h.alias == alias) {
            self.table.select(Some(pos));
        }
    }

    fn key_confirm(&mut self, key: KeyEvent) {
        let Mode::ConfirmDelete(i) = self.mode else {
            return;
        };
        if matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y')) {
            let h = self.store.hosts.remove(i);
            self.persist_or_report(&t!("st.removed", alias = h.alias));
        }
        self.mode = Mode::List;
        self.clamp_selection();
    }

    fn key_lang(&mut self, key: KeyEvent) {
        let Mode::Lang(i) = self.mode else {
            return;
        };
        let n = lang::LANGS.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.mode = Mode::Lang((i + n - 1) % n),
            KeyCode::Down | KeyCode::Char('j') => self.mode = Mode::Lang((i + 1) % n),
            KeyCode::Enter => {
                let code = lang::LANGS[i];
                lang::set(code);
                self.store.lang = Some(code.to_string());
                self.mode = Mode::List;
                self.persist_or_report(&t!("st.lang"));
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::List,
            _ => {}
        }
    }

    fn import(&mut self) {
        let found = match sshconfig::import_existing() {
            Ok(f) => f,
            Err(e) => return self.set_status(t!("st.import_error", err = format!("{e:#}"))),
        };
        let mut added = 0;
        for h in found {
            if !self.store.hosts.iter().any(|x| x.alias == h.alias) {
                self.store.hosts.push(Host {
                    created: Some(vault::now()),
                    ..h
                });
                added += 1;
            }
        }
        if added == 0 {
            self.set_status(t!("st.import_none"));
        } else {
            self.persist_or_report(&t!("st.imported", n = added));
            self.clamp_selection();
        }
    }
}
