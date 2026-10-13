mod app;
mod askpass;
mod clipboard;
mod lang;
mod rdp;
mod session;
mod sshconfig;
mod ui;
mod vault;

rust_i18n::i18n!("locales", fallback = "en");

use anyhow::Result;
use app::{App, External};
use ratatui::{
    DefaultTerminal,
    crossterm::{
        cursor,
        event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind},
        execute, terminal,
    },
};
use rust_i18n::t;
use std::{
    io::{BufRead, stdout},
    process::Command,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// Intervalo mínimo entre redesenhos causados por saída do ssh (~120 fps).
const FRAME: Duration = Duration::from_millis(8);

fn main() -> Result<()> {
    // chamado pelo ssh como SSH_ASKPASS
    if let Some(code) = askpass::run_if_requested() {
        std::process::exit(code);
    }

    lang::set(lang::detect());

    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = run(&mut terminal);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    let _ = execute!(stdout(), cursor::Show);
    result
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::new();
    let mut needs_draw = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        // redesenha só quando algo mudou: tecla, saída do ssh, ou 1x/s (relógio e status)
        let since = last_draw.elapsed();
        let output = since >= FRAME && app.output.swap(false, Ordering::Relaxed);
        if needs_draw || output || since >= Duration::from_secs(1) {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            last_draw = Instant::now();
            needs_draw = false;
        }

        let timeout = if !app.sessions.is_empty() {
            4
        } else if app.rdp_connecting() {
            20
        } else {
            250
        };
        if event::poll(Duration::from_millis(timeout))? {
            // trata todos os eventos pendentes antes de redesenhar (digitação rápida, colar)
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                    Event::Paste(text) => app.on_paste(&text),
                    _ => {}
                }
                if app.quit || app.pending.is_some() || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            needs_draw = true;
        }
        let snapshot = |a: &App| (a.open_count(), a.status.as_ref().map(|s| s.1));
        let before = snapshot(&app);
        app.tick();
        needs_draw |= before != snapshot(&app);

        if let Some(ext) = app.pending.take() {
            let msg: String = match ext {
                External::Ssh(alias) => {
                    let status = suspended(terminal, || {
                        println!("{}\n", t!("ext.ssh_banner", alias = alias));
                        Command::new("ssh").arg(&alias).status()
                    })?;
                    match status {
                        Ok(s) if s.success() => t!("ext.ssh_ended", alias = alias).into(),
                        Ok(s) => {
                            t!("ext.ssh_exit", alias = alias, code = s.code().unwrap_or(-1)).into()
                        }
                        Err(e) => t!("ext.ssh_failed", err = e).into(),
                    }
                }
                External::CopyId {
                    alias,
                    identity,
                    password,
                } => {
                    let status = suspended(terminal, || copy_id(&alias, &identity, &password))?;
                    match status {
                        Ok(true) => t!("ext.copyid_ok", alias = alias).into(),
                        Ok(false) => t!("ext.copyid_failed", alias = alias).into(),
                        Err(e) => t!("ext.copyid_error", err = e).into(),
                    }
                }
            };
            app.set_status(msg);
            needs_draw = true;
        }
    }
    Ok(())
}

/// Sai da TUI, executa `f` no terminal normal e volta.
fn suspended<T>(terminal: &mut DefaultTerminal, f: impl FnOnce() -> T) -> Result<T> {
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    // o ratatui esconde o cursor enquanto desenha; o ssh precisa dele visível
    execute!(stdout(), cursor::Show)?;
    let out = f();
    *terminal = ratatui::init();
    // limpa direto: terminal.clear() consulta a posição do cursor e pode estourar timeout
    execute!(
        stdout(),
        EnableBracketedPaste,
        terminal::Clear(terminal::ClearType::All)
    )?;
    Ok(out)
}

/// Roda ssh-copy-id preenchendo a senha salva via SSH_ASKPASS (o próprio sshdeck).
fn copy_id(alias: &str, identity: &str, password: &str) -> std::io::Result<bool> {
    let mut cmd = Command::new("ssh-copy-id");
    if !identity.is_empty() {
        // sem shell, o ~ não é expandido
        let path = match identity.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => identity.into(),
        };
        cmd.arg("-i").arg(path);
    }
    cmd.arg(alias);
    if !password.is_empty() {
        cmd.env("SSH_ASKPASS", std::env::current_exe()?)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(askpass::PASSWORD_ENV, password);
    }

    println!("→ ssh-copy-id {alias}\n");
    let ok = cmd.status()?.success();
    println!("\n{}", if ok { t!("ext.done") } else { t!("ext.failed") });
    let _ = std::io::stdin().lock().read_line(&mut String::new());
    Ok(ok)
}
