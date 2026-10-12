use crate::{
    clipboard::Clip,
    rdp::{self, RdpSession},
    session::Session,
    sshconfig,
    vault::{Host, Kind, Store, Vault},
};
use anyhow::{Result, bail};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    widgets::TableState,
};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq)]
pub enum PwPurpose {
    Unlock,
    Create,
    Rekey,
}

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
    Help,
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
    pub fn label(self) -> &'static str {
        match self {
            Field::Kind => "Tipo",
            Field::Alias => "Alias",
            Field::HostName => "HostName / IP",
            Field::User => "User",
            Field::Domain => "Domínio",
            Field::Port => "Port",
            Field::IdentityFile => "IdentityFile",
            Field::Resolution => "Resolução",
            Field::Share => "Pasta compart.",
            Field::Gateway => "Túnel SSH via",
            Field::Password => "Password",
            Field::Notes => "Notas",
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
            bail!("Alias é obrigatório");
        }
        if alias.contains(char::is_whitespace) || alias.contains(['*', '?', '!', '#', '/']) {
            bail!("Alias não pode ter espaços nem * ? ! # /");
        }
        let hostname = f(Field::HostName);
        if hostname.is_empty() || hostname.contains(char::is_whitespace) {
            bail!("HostName / IP inválido");
        }
        let port = if f(Field::Port).is_empty() {
            self.default_port()
        } else {
            f(Field::Port)
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("Porta inválida"))?
        };
        let rdp = self.kind == Kind::Rdp;
        if rdp && f(Field::User).is_empty() {
            bail!("RDP precisa de User");
        }
        if rdp && !rdp::valid_resolution(&f(Field::Resolution)) {
            bail!("Resolução inválida: use vazio, full, 1920x1080 ou 90%");
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
    /// sessões ssh embutidas (workspace)
    pub sessions: Vec<Session>,
    /// janelas do FreeRDP abertas pelo sshdeck
    pub rdp_sessions: Vec<RdpSession>,
    pub active: usize,
    /// teclas vão para o terminal da sessão ativa
    pub term_focus: bool,
    /// tamanho do painel do terminal, calculado pela UI
    pub term_size: (u16, u16),
    /// marcado pelas sessões quando chega saída nova
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
            active: 0,
            term_focus: false,
            term_size: (24, 80),
            output: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    pub fn tick(&mut self) {
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
                        Ok(()) => self.set_status(format!("RDP {} fechado", s.alias)),
                        Err(e) => self.set_status(format!("RDP {}: {e}", s.alias)),
                    }
                }
            }
        }

        // remove sessões cujo ssh terminou
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
            self.set_status(format!("Sessão encerrada: {}", closed.join(", ")));
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

    pub fn has_session(&self, alias: &str) -> bool {
        self.sessions.iter().any(|s| s.alias == alias)
            || self.rdp_sessions.iter().any(|s| s.alias == alias)
    }

    /// Total de conexões abertas (ssh embutido + janelas RDP).
    pub fn open_count(&self) -> usize {
        self.sessions.len() + self.rdp_sessions.len()
    }

    fn open_rdp(&mut self, idx: usize) {
        let host = &self.store.hosts[idx];
        if self.rdp_sessions.iter().any(|s| s.alias == host.alias) {
            return self.set_status(format!("RDP {} já está aberto", host.alias));
        }
        let gateway = (!host.gateway.is_empty())
            .then(|| self.host_by_alias(&host.gateway))
            .flatten();
        if !host.gateway.is_empty() && gateway.is_none() {
            return self.set_status(format!("Túnel SSH via '{}': host não existe", host.gateway));
        }
        match RdpSession::start(host, gateway) {
            Ok(s) => {
                self.set_status(if s.via_tunnel() {
                    format!("Abrindo túnel via {} e RDP {}…", host.gateway, host.alias)
                } else {
                    format!("Abrindo RDP {}…", host.alias)
                });
                self.rdp_sessions.push(s);
            }
            Err(e) => self.set_status(format!("Falha no RDP: {e:#}")),
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
                }
                Err(e) => return self.set_status(format!("Falha ao conectar: {e:#}")),
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

    /// Seleciona na lista o host da sessão ativa.
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
                self.set_status(format!("Senha de {alias} digitada — aperte Enter"));
            }
            _ => self.set_status(format!("{alias} não tem senha salva")),
        }
    }

    /// Índices de `store.hosts` que passam pelo filtro.
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

    fn selected_host(&self) -> Option<usize> {
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

    fn persist_or_report(&mut self, ok_msg: &str) {
        match self.persist() {
            Ok(()) => self.set_status(ok_msg),
            Err(e) => self.set_status(format!("Erro ao salvar: {e:#}")),
        }
    }

    fn copy_password(&mut self, idx: usize) -> bool {
        let h = &self.store.hosts[idx];
        if h.password.is_empty() {
            self.set_status(format!("{} não tem senha salva", h.alias));
            return false;
        }
        let pw = h.password.clone();
        let alias = h.alias.clone();
        let how = self.clip.copy_secret(&pw);
        self.set_status(format!(
            "Senha de {alias} copiada ({how}) — limpa em {}s",
            crate::clipboard::CLEAR_AFTER.as_secs()
        ));
        true
    }

    // ---------- teclado ----------

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
            Mode::ConfirmQuit => {
                if matches!(key.code, KeyCode::Char('s') | KeyCode::Char('y')) {
                    self.quit = true;
                } else {
                    self.mode = Mode::List;
                }
            }
            Mode::Help => self.mode = Mode::List,
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
                            self.clamp_selection();
                            // garante que o ssh config reflete o cofre
                            if let Err(e) = sshconfig::write(&self.store.hosts) {
                                self.set_status(format!("Erro no ssh config: {e:#}"));
                            }
                        }
                        Err(e) => *error = Some(format!("{e:#}")),
                    }
                    return;
                }
                // Create / Rekey: pede duas vezes
                if typed.len() < 4 {
                    *error = Some("Use pelo menos 4 caracteres".into());
                    return;
                }
                match first.take() {
                    None => {
                        *first = Some(typed);
                        *error = None;
                    }
                    Some(f) if *f != *typed => {
                        *error = Some("As senhas não conferem, tente de novo".into());
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
                                    "Cofre criado. Aperte 'a' para adicionar um host ou 'i' para importar do ~/.ssh/config"
                                } else {
                                    "Senha mestra alterada"
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
                        self.set_status(format!("Sessão {alias} fechada"));
                    } else if let Some(pos) =
                        self.rdp_sessions.iter().position(|s| s.alias == alias)
                    {
                        // Drop fecha a janela do FreeRDP e o túnel
                        self.rdp_sessions.remove(pos);
                        self.set_status(format!("RDP {alias} fechado"));
                    }
                }
            }
            KeyCode::Char('s') | KeyCode::Char('K')
                if self
                    .selected_host()
                    .is_some_and(|i| self.store.hosts[i].kind == Kind::Rdp) =>
            {
                self.set_status("Esse atalho é só para hosts SSH");
            }
            KeyCode::Char('s') => {
                if let Some(i) = self.selected_host() {
                    self.copy_password(i);
                    self.pending = Some(External::Ssh(self.store.hosts[i].alias.clone()));
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
            f.error = Some(format!("Já existe um host com alias '{}'", host.alias));
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
                f.error = Some(format!(
                    "Túnel SSH via: '{}' não é um host SSH cadastrado",
                    host.gateway
                ));
                return;
            }
        }
        let alias = host.alias.clone();
        match editing {
            Some(i) => self.store.hosts[i] = host,
            None => self.store.hosts.push(host),
        }
        self.mode = Mode::List;
        self.persist_or_report(&format!("'{alias}' salvo"));
        // reposiciona a seleção no host salvo
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
            self.persist_or_report(&format!("'{}' removido", h.alias));
        }
        self.mode = Mode::List;
        self.clamp_selection();
    }

    fn import(&mut self) {
        let found = match sshconfig::import_existing() {
            Ok(f) => f,
            Err(e) => return self.set_status(format!("Erro ao importar: {e:#}")),
        };
        let mut added = 0;
        for h in found {
            if !self.store.hosts.iter().any(|x| x.alias == h.alias) {
                self.store.hosts.push(h);
                added += 1;
            }
        }
        if added == 0 {
            self.set_status("Nada novo para importar do ~/.ssh/config");
        } else {
            self.persist_or_report(&format!(
                "{added} host(s) importado(s) — use 'e' para adicionar as senhas"
            ));
            self.clamp_selection();
        }
    }
}
