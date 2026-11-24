//! Cadastro de hosts: validação do formulário, salvar (novo ou edição) e importar do
//! `~/.ssh/config`.

use crate::{
    rdp, sshconfig,
    vault::{self, Host, Kind, Store},
};
use anyhow::{Result, bail};
use rust_i18n::t;
use serde::{Deserialize, Serialize};

/// Os campos do formulário, como digitados (a porta ainda é texto).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostForm {
    pub kind: Kind,
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity_file: String,
    pub password: String,
    pub notes: String,
    pub domain: String,
    pub resolution: String,
    pub gateway: String,
}

pub fn default_port(kind: Kind) -> u16 {
    match kind {
        Kind::Ssh => 22,
        Kind::Rdp => rdp::DEFAULT_PORT,
    }
}

impl HostForm {
    pub fn from_host(h: &Host) -> Self {
        Self {
            kind: h.kind,
            alias: h.alias.clone(),
            hostname: h.hostname.clone(),
            user: h.user.clone(),
            port: h.port.to_string(),
            identity_file: h.identity_file.clone(),
            password: h.password.clone(),
            notes: h.notes.clone(),
            domain: h.domain.clone(),
            resolution: h.resolution.clone(),
            gateway: h.gateway.clone(),
        }
    }

    /// Valida e monta o host. Campos que não pertencem ao tipo não são guardados.
    pub fn to_host(&self) -> Result<Host> {
        let alias = self.alias.trim().to_string();
        if alias.is_empty() {
            bail!("{}", t!("err.alias_required"));
        }
        if alias.contains(char::is_whitespace) || alias.contains(['*', '?', '!', '#', '/']) {
            bail!("{}", t!("err.alias_chars"));
        }
        let hostname = self.hostname.trim().to_string();
        if hostname.is_empty() || hostname.contains(char::is_whitespace) {
            bail!("{}", t!("err.hostname"));
        }
        let port = match self.port.trim() {
            "" => default_port(self.kind),
            p => p
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("{}", t!("err.port")))?,
        };
        let rdp = self.kind == Kind::Rdp;
        let user = self.user.trim().to_string();
        if rdp && user.is_empty() {
            bail!("{}", t!("err.rdp_user"));
        }
        if rdp && !rdp::valid_resolution(self.resolution.trim()) {
            bail!("{}", t!("err.resolution"));
        }
        let only = |cond: bool, v: &str| if cond { v.trim().to_string() } else { String::new() };
        Ok(Host {
            kind: self.kind,
            alias,
            hostname,
            user,
            port,
            identity_file: only(!rdp, &self.identity_file),
            // senha não é trimada: espaços podem fazer parte dela
            password: self.password.clone(),
            notes: self.notes.trim().to_string(),
            domain: only(rdp, &self.domain),
            resolution: only(rdp, &self.resolution),
            gateway: only(rdp, &self.gateway),
            ..Default::default()
        })
    }
}

/// Salva um host novo (`editing = None`) ou a edição do host na posição `editing`.
/// Na edição, o que não está no formulário (datas, informações coletadas) é mantido.
pub fn upsert(store: &mut Store, editing: Option<usize>, host: Host) -> Result<()> {
    let dup = store
        .hosts
        .iter()
        .enumerate()
        .any(|(i, h)| Some(i) != editing && h.alias == host.alias);
    if dup {
        bail!("{}", t!("err.duplicate", alias = host.alias));
    }
    if !host.gateway.is_empty() {
        let ok = host.gateway != host.alias
            && store
                .hosts
                .iter()
                .any(|h| h.alias == host.gateway && h.kind == Kind::Ssh);
        if !ok {
            bail!("{}", t!("err.gateway", gw = host.gateway));
        }
    }
    match editing {
        Some(i) => {
            let old = std::mem::take(&mut store.hosts[i]);
            store.hosts[i] = Host {
                last_used: old.last_used,
                created: old.created,
                updated: Some(vault::now()),
                sysinfo_allowed: old.sysinfo_allowed,
                sysinfo: old.sysinfo,
                rdp_cert: old.rdp_cert,
                ..host
            };
        }
        None => store.hosts.push(Host {
            created: Some(vault::now()),
            ..host
        }),
    }
    Ok(())
}

/// Traz os hosts do `~/.ssh/config` que ainda não estão no cofre. Devolve quantos entraram.
pub fn import(store: &mut Store) -> Result<usize> {
    let mut added = 0;
    for h in sshconfig::import_existing()? {
        if !store.hosts.iter().any(|x| x.alias == h.alias) {
            store.hosts.push(Host {
                created: Some(vault::now()),
                ..h
            });
            added += 1;
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(alias: &str) -> HostForm {
        HostForm {
            alias: alias.into(),
            hostname: "10.0.0.1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn validation_and_upsert() {
        assert!(form("").to_host().is_err());
        assert!(form("a b").to_host().is_err());
        let h = form("prod").to_host().unwrap();
        assert_eq!(h.port, 22);
        let mut rdp = form("win");
        rdp.kind = Kind::Rdp;
        assert!(rdp.to_host().is_err()); // RDP sem usuário
        rdp.user = "adm".into();
        assert_eq!(rdp.to_host().unwrap().port, rdp::DEFAULT_PORT);

        let mut store = Store::default();
        upsert(&mut store, None, h.clone()).unwrap();
        assert!(upsert(&mut store, None, h.clone()).is_err()); // duplicado
        store.hosts[0].last_used = Some(5);
        let mut edited = h;
        edited.user = "root".into();
        upsert(&mut store, Some(0), edited).unwrap();
        assert_eq!(store.hosts[0].user, "root");
        assert_eq!(store.hosts[0].last_used, Some(5));
        assert!(store.hosts[0].updated.is_some());
    }
}
