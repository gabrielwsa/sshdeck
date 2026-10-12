# clissh

Gestor de SSH no terminal (Rust + ratatui): cadastra hosts (alias, IP, user, porta, chave),
gera o `~/.ssh/config` e guarda as senhas criptografadas para colar no servidor (`sudo su` etc.).

## Instalar

```sh
cargo install --path .
clissh
```

Use uma [Nerd Font](https://www.nerdfonts.com/) no terminal: o ícone  (`nf-fa-key`) marca os
hosts com senha salva.

## Como funciona

- **Cofre**: `~/.local/share/clissh/vault.bin`, criptografado com a senha mestra
  (Argon2id + XChaCha20-Poly1305). Hosts e senhas ficam só ali.
- **SSH config**: o clissh gera `~/.ssh/clissh.conf` e coloca `Include ~/.ssh/clissh.conf`
  no topo do `~/.ssh/config` (backup em `~/.ssh/config.clissh.bak`). Assim `ssh <alias>`
  funciona em qualquer lugar, até fora do clissh. Se o mesmo alias existir nos dois arquivos,
  vale o do clissh.
- **Workspace**: `Enter` abre o ssh dentro do clissh — servidores à esquerda (● = conectado),
  informações da conexão (user, host, porta, tempo, notas) embaixo, e o terminal no centro.
  Dá pra ter várias sessões abertas ao mesmo tempo.
- **Senha**: no terminal, `Alt+P` digita a senha salva direto (ex.: `sudo su` → `Alt+P` → `Enter`).
  `Enter` na lista também copia a senha pro clipboard (limpo após 45s; sem clipboard do
  sistema, usa OSC 52).

## Teclas

| Tecla | Ação |
|---|---|
| `Enter` | conecta no painel central (ou vai para a sessão já aberta) |
| `s` | conecta em tela cheia (ssh normal, volta ao sair) |
| `Tab` / `l` | foca o terminal |
| `x` | fecha a sessão do host selecionado |
| `K` | `ssh-copy-id` no host (preenche a senha salva sozinho) |
| `c` / `y` | copia a senha |
| `a` / `e` / `d` | adicionar / editar / apagar |
| `/` | filtrar |
| `i` | importar hosts do `~/.ssh/config` |
| `P` | trocar a senha mestra |
| `?` | ajuda |
| `q` | sair |

No terminal: `Ctrl+]` volta para o painel (a sessão continua), `Alt+P` digita a senha,
`Alt+←/→` troca de sessão, `Shift+PgUp/PgDn` rola o histórico.

No formulário: `Tab` navega, `Ctrl+S` salva, `Ctrl+R` mostra a senha, `Ctrl+U` limpa o campo.
