//! Terminal ssh para o xterm.js: o ssh roda num PTY e os bytes vão crus para a janela,
//! que faz a emulação. O caminho de volta (teclas, colar) chega pelo `write`.

use anyhow::{Result, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{io::Read, io::Write, thread};
use tauri::ipc::{Channel, InvokeResponseBody};

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Pty {
    /// `ssh alias` no PTY.
    pub fn ssh(
        alias: &str,
        rows: u16,
        cols: u16,
        on_data: Channel<InvokeResponseBody>,
        on_exit: Channel<()>,
    ) -> Result<Self> {
        let mut cmd = CommandBuilder::new("ssh");
        cmd.arg(alias);
        Self::spawn(cmd, rows, cols, on_data, on_exit)
    }

    /// Roda `cmd` no PTY. `on_data` recebe a saída; `on_exit` avisa quando ele termina.
    pub fn spawn(
        mut cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        on_data: Channel<InvokeResponseBody>,
        on_exit: Channel<()>,
    ) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(size(rows, cols))
            .map_err(|e| anyhow!("{e}"))?;
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(home) = std::env::var_os("HOME") {
            cmd.cwd(home);
        }
        let child = pair.slave.spawn_command(cmd).map_err(|e| anyhow!("{e}"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
        let writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;
        thread::spawn(move || {
            let mut buf = [0u8; 16384];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if on_data
                            .send(InvokeResponseBody::Raw(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = on_exit.send(());
        });
        Ok(Self {
            master: pair.master,
            writer,
            child,
        })
    }

    pub fn write(&mut self, data: &[u8]) {
        let _ = self.writer.write_all(data);
        let _ = self.writer.flush();
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.master.resize(size(rows, cols));
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(2),
        cols: cols.max(10),
        pixel_width: 0,
        pixel_height: 0,
    }
}

