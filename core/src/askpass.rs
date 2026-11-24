//! O próprio sshdeck serve de SSH_ASKPASS: quando o ssh (ou ssh-copy-id) precisa da senha,
//! chama o executável do sshdeck com o prompt como argumento, e ele responde com a senha
//! recebida pela variável de ambiente. Outras perguntas vão para o usuário no terminal.

use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    process::{Command, Stdio},
};

pub const PASSWORD_ENV: &str = "SSHDECK_ASKPASS_PW";
/// definido quando não há terminal livre (túnel do RDP em segundo plano):
/// qualquer pergunta que não seja a senha falha em vez de esperar resposta
pub const NO_TTY_ENV: &str = "SSHDECK_ASKPASS_NOTTY";

/// Se o processo foi chamado como askpass, responde e devolve o código de saída.
pub fn run_if_requested() -> Option<i32> {
    let password = std::env::var(PASSWORD_ENV).ok()?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let lower = prompt.to_lowercase();

    // "user@host's password:" — mas não "passphrase" de chave privada
    let answer = if lower.contains("password") && !lower.contains("passphrase") {
        Ok(password)
    } else if std::env::var_os(NO_TTY_ENV).is_some() {
        Err(io::Error::other("sem terminal para responder"))
    } else {
        // ex.: confirmar fingerprint de host novo (yes/no), passphrase de chave
        let secret = lower.contains("passphrase") || lower.contains("pin");
        ask_tty(&prompt, secret)
    };
    match answer {
        Ok(a) => {
            println!("{a}");
            Some(0)
        }
        Err(_) => Some(1),
    }
}

fn ask_tty(prompt: &str, secret: bool) -> io::Result<String> {
    let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    write!(tty, "{prompt}")?;
    tty.flush()?;
    if secret {
        stty("-echo");
    }
    let mut line = String::new();
    let read = BufReader::new(tty.try_clone()?).read_line(&mut line);
    if secret {
        stty("echo");
        writeln!(tty)?;
    }
    read?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

fn stty(arg: &str) {
    if let Ok(tty) = File::open("/dev/tty") {
        let _ = Command::new("stty")
            .arg(arg)
            .stdin(tty)
            .stdout(Stdio::null())
            .status();
    }
}
