//! Sessão ssh embutida: o ssh roda num PTY e a saída alimenta um emulador vt100
//! que a TUI desenha no painel central.

use anyhow::{Result, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_i18n::t;
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

const SCROLLBACK: usize = 5000;

pub struct Session {
    pub alias: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub started: Instant,
    exited: Arc<AtomicBool>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
}

impl Session {
    /// `notify` é marcado sempre que chega saída nova, para a TUI saber que precisa redesenhar.
    pub fn spawn(alias: &str, rows: u16, cols: u16, notify: Arc<AtomicBool>) -> Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("{}", t!("session.pty", err = e)))?;

        let mut cmd = CommandBuilder::new("ssh");
        cmd.arg(alias);
        cmd.env("TERM", "xterm-256color");
        if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("{}", t!("session.spawn", err = e)))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
        let writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let exited = Arc::new(AtomicBool::new(false));

        let (p, ex) = (parser.clone(), exited.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        p.lock().unwrap().process(&buf[..n]);
                        notify.store(true, Ordering::Relaxed);
                    }
                }
            }
            ex.store(true, Ordering::Relaxed);
            notify.store(true, Ordering::Relaxed);
        });

        Ok(Self {
            alias: alias.to_string(),
            parser,
            started: Instant::now(),
            exited,
            master: pair.master,
            writer,
            child,
            size: (rows, cols),
        })
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        if self.size == (rows, cols) {
            return;
        }
        self.size = (rows, cols);
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        let app_cursor = {
            let mut p = self.parser.lock().unwrap();
            // qualquer digitação volta para o fim do scrollback
            p.screen_mut().set_scrollback(0);
            p.screen().application_cursor()
        };
        if let Some(bytes) = encode_key(key, app_cursor) {
            self.write(&bytes);
        }
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
        if bracketed {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    /// Roda do mouse. Em tela alternativa (less, vim, htop) não há histórico: manda ↑/↓,
    /// como os terminais fazem; no shell, rola o histórico.
    pub fn wheel(&mut self, up: bool) {
        const LINES: usize = 3;
        let (alternate, app_cursor) = {
            let p = self.parser.lock().unwrap();
            (
                p.screen().alternate_screen(),
                p.screen().application_cursor(),
            )
        };
        if alternate {
            let arrow = match (up, app_cursor) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.write(arrow.repeat(LINES).as_bytes());
        } else {
            self.scroll(if up {
                LINES as isize
            } else {
                -(LINES as isize)
            });
        }
    }

    /// Rola o histórico: positivo sobe, negativo desce.
    pub fn scroll(&mut self, delta: isize) {
        let mut p = self.parser.lock().unwrap();
        let cur = p.screen().scrollback() as isize;
        p.screen_mut().set_scrollback((cur + delta).max(0) as usize);
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Converte uma tecla do crossterm na sequência de bytes que um xterm enviaria.
fn encode_key(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    let arrow = |c: char| {
        if app_cursor {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
        .into_bytes()
    };

    let mut out = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' => 0x1f,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => csi("Z"),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow('A'),
        KeyCode::Down => arrow('B'),
        KeyCode::Right => arrow('C'),
        KeyCode::Left => arrow('D'),
        KeyCode::Home => arrow('H'),
        KeyCode::End => arrow('F'),
        KeyCode::Insert => csi("2~"),
        KeyCode::Delete => csi("3~"),
        KeyCode::PageUp => csi("5~"),
        KeyCode::PageDown => csi("6~"),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => csi("15~"),
            6 => csi("17~"),
            7 => csi("18~"),
            8 => csi("19~"),
            9 => csi("20~"),
            10 => csi("21~"),
            11 => csi("23~"),
            12 => csi("24~"),
            _ => return None,
        },
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}
