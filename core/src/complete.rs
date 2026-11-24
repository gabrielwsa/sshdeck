//! Autocomplete dos caminhos do popup do rsync. No servidor, uma conexão ssh fica aberta
//! enquanto o popup existe e responde cada `ls` na hora; no local, lê o disco direto.

use crate::{askpass, transfer::expand_tilde, vault::Host};
use anyhow::Result;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

/// Máximo de sugestões mostradas, para pastas enormes não travarem a tela.
pub const LIMIT: usize = 200;
const BEGIN: &str = "@@sshdeck-ls@@";
const END: &str = "@@sshdeck-end@@";

pub enum Listing {
    Loading,
    Ready(Vec<String>),
    Failed(String),
}

type Results = Arc<Mutex<HashMap<u64, Vec<String>>>>;

pub struct RemoteLister {
    child: Child,
    stdin: ChildStdin,
    next_id: u64,
    /// pasta pedida → id da resposta
    ids: HashMap<String, u64>,
    results: Results,
    /// conexão caiu (ou nem abriu): o motivo, tirado do stderr do ssh
    failed: Arc<Mutex<Option<String>>>,
}

impl RemoteLister {
    /// `wake` é avisado a cada resposta, para a tela redesenhar.
    pub fn start(host: &Host, wake: Arc<AtomicBool>) -> Result<Self> {
        let mut child = Command::new("ssh")
            .args(["-T", "-o", "ConnectTimeout=10"])
            // em segundo plano não há quem responda sobre chave nova do servidor
            .args(["-o", "StrictHostKeyChecking=yes"])
            .arg(&host.alias)
            .arg("sh")
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env(askpass::NO_TTY_ENV, "1")
            .env(askpass::PASSWORD_ENV, &host.password)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");

        let results: Results = Arc::default();
        let failed = Arc::new(Mutex::new(None));
        let err_text = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let (out, fail) = (results.clone(), failed.clone());
        thread::spawn(move || {
            let mut current: Option<(u64, Vec<String>)> = None;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(id) = line.strip_prefix(BEGIN) {
                    current = id.trim().parse().ok().map(|id| (id, Vec::new()));
                } else if line == END {
                    if let Some((id, entries)) = current.take() {
                        out.lock().unwrap().insert(id, entries);
                        wake.store(true, Ordering::Relaxed);
                    }
                } else if let Some((_, entries)) = &mut current {
                    entries.push(line);
                }
            }
            let text = err_text.join().unwrap_or_default();
            let reason = text
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .unwrap_or("ssh")
                .to_string();
            *fail.lock().unwrap() = Some(reason);
            wake.store(true, Ordering::Relaxed);
        });

        Ok(Self {
            child,
            stdin,
            next_id: 0,
            ids: HashMap::new(),
            results,
            failed,
        })
    }

    /// Pede a listagem da pasta, se ainda não foi pedida.
    pub fn request(&mut self, dir: &str) {
        if self.ids.contains_key(dir) || self.failed.lock().unwrap().is_some() {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        // sem pasta = home do usuário, onde o `sh` do ssh já começa
        let path = if dir.is_empty() { "." } else { dir };
        let cmd = format!(
            "echo '{BEGIN}{id}'; ls -1Ap -- {} 2>/dev/null | head -n 5000; echo '{END}'\n",
            shell_quote(path)
        );
        if self.stdin.write_all(cmd.as_bytes()).is_ok() {
            let _ = self.stdin.flush();
            self.ids.insert(dir.to_string(), id);
        }
    }

    pub fn get(&self, dir: &str) -> Listing {
        if let Some(id) = self.ids.get(dir)
            && let Some(entries) = self.results.lock().unwrap().get(id)
        {
            return Listing::Ready(entries.clone());
        }
        match self.failed.lock().unwrap().as_ref() {
            Some(reason) => Listing::Failed(reason.clone()),
            None => Listing::Loading,
        }
    }
}

impl Drop for RemoteLister {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pasta local, com `/` no fim das subpastas (como o `ls -p`).
pub fn local(dir: &str) -> Listing {
    let path = if dir.is_empty() {
        std::path::PathBuf::from(".")
    } else {
        expand_tilde(dir)
    };
    match std::fs::read_dir(&path) {
        Ok(rd) => {
            let mut entries: Vec<String> = rd
                .flatten()
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    // segue links: link para pasta também ganha a barra
                    if e.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            entries.sort();
            Listing::Ready(entries)
        }
        Err(e) => Listing::Failed(e.to_string()),
    }
}

/// Separa o que foi digitado em pasta (até a última `/`, inclusive) e o começo do nome.
pub fn split(input: &str) -> (&str, &str) {
    match input.rfind('/') {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    }
}

/// Pasta do servidor como o `ls` recebe: `~/x/` vira relativo ao home.
pub fn remote_dir(dir: &str) -> &str {
    dir.strip_prefix("~/").unwrap_or(dir)
}

/// Nomes que começam com o que foi digitado; ocultos só se o começo já for um ponto.
pub fn matches<'a>(entries: &'a [String], prefix: &str) -> Vec<&'a str> {
    entries
        .iter()
        .map(String::as_str)
        .filter(|e| e.starts_with(prefix) && (prefix.starts_with('.') || !e.starts_with('.')))
        .take(LIMIT)
        .collect()
}

/// Maior começo em comum entre as sugestões (o que o Tab completa sozinho).
pub fn common_prefix(names: &[&str]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut len = first.len();
    for n in &names[1..] {
        len = first
            .char_indices()
            .zip(n.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, c), _)| i + c.len_utf8())
            .min(len);
    }
    first[..len].to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_input() {
        assert_eq!(split("/var/log/ng"), ("/var/log/", "ng"));
        assert_eq!(split("/var/log/"), ("/var/log/", ""));
        assert_eq!(split("docs"), ("", "docs"));
        assert_eq!(split(""), ("", ""));
        assert_eq!(remote_dir("~/proj/"), "proj/");
    }

    #[test]
    fn matching_and_completion() {
        let entries: Vec<String> = ["nginx/", "ngrok.log", ".hidden", "apt/"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(matches(&entries, "ng"), ["nginx/", "ngrok.log"]);
        assert_eq!(matches(&entries, ""), ["nginx/", "ngrok.log", "apt/"]);
        assert_eq!(matches(&entries, "."), [".hidden"]);
        assert_eq!(common_prefix(&["nginx/", "ngrok.log"]), "ng");
        assert_eq!(common_prefix(&["nginx/"]), "nginx/");
        assert_eq!(common_prefix(&["ação/", "açúcar"]), "aç");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
