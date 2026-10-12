# sshdeck

Gestor de SSH e RDP no terminal (Rust + ratatui): cadastra hosts (alias, IP, user, porta, chave),
gera o `~/.ssh/config` e guarda as senhas criptografadas para usar no servidor (`sudo su` etc.).
Hosts Windows abrem por RDP (FreeRDP), com a senha preenchida e túnel SSH opcional.

## Instalar

```sh
cargo install --path .
sshdeck
```

Use uma [Nerd Font](https://www.nerdfonts.com/) no terminal: o ícone  (`nf-fa-key`) marca os
hosts com senha salva,  (`nf-fa-terminal`) os hosts SSH e  (`nf-fa-windows`) os RDP.

Para RDP, instale o FreeRDP 3 (`pacman -S freerdp`).

## Onde ficam as coisas

| O quê | Onde | Permissão | Criptografado? |
|---|---|---|---|
| **Cofre** (todos os hosts + senhas + notas) | `~/.local/share/sshdeck/vault.bin` (ou `$XDG_DATA_HOME/sshdeck/`) | pasta `700`, arquivo `600` | **Sim** |
| Config SSH gerada | `~/.ssh/sshdeck.conf` | `600` | Não, mas **não tem senhas**: só Host, HostName, User, Port e IdentityFile |
| Seu config SSH | `~/.ssh/config`: o sshdeck só coloca `Include ~/.ssh/sshdeck.conf` na 1ª linha | `600` | Não |
| Backup do seu config | `~/.ssh/config.sshdeck.bak`, criado uma única vez, antes da 1ª alteração | | Não |
| Logs do RDP | `~/.cache/sshdeck/rdp-<alias>.log` (mensagens do FreeRDP e do túnel) | pasta `700`, arquivo `600` | Não, mas **não tem senhas** |
| Certificados RDP aceitos | `~/.config/freerdp/` (gerenciado pelo próprio FreeRDP) | | Não |

Não existe banco de dados separado: **o cofre é o banco**. É um único arquivo com tudo dentro,
criptografado inteiro. Ou seja, nem os nomes e IPs dos servidores ficam legíveis nele, só no
`sshdeck.conf`, que o ssh precisa ler.

Com o `Include` no topo, `ssh <alias>` funciona em qualquer lugar, até fora do sshdeck. Se o mesmo
alias existir nos dois arquivos, vale o do sshdeck.

## Criptografia do cofre

- **Derivação da chave:** **Argon2id**, com 19 MiB de memória, 2 passadas e 1 thread (os padrões
  recomendados da biblioteca). Ele transforma a senha mestra numa chave de 256 bits. O salt é
  aleatório, de 16 bytes, e é renovado quando você troca a senha mestra.
- **Cifra:** **XChaCha20-Poly1305**, criptografia autenticada. Cada vez que o cofre é salvo, ele usa
  um nonce aleatório novo de 24 bytes. Qualquer alteração no arquivo, ou uma senha errada, faz a
  abertura falhar ("senha mestra incorreta"). O cofre nunca abre corrompido.
- **Formato do arquivo:** `SSHDK1` (6 bytes) | salt (16) | nonce (24) | dados criptografados (JSON
  dos hosts) + tag de autenticação (16).
- **Gravação segura:** ele escreve primeiro em `vault.tmp`, força a gravação no disco e só então
  renomeia. Se o computador desligar no meio, o cofre antigo continua inteiro.
- **Senha mestra:** mínimo de 4 caracteres, e você digita duas vezes ao criar. **Não existe
  recuperação.** Se esquecer, perde o cofre.

## Funcionalidades

### Hosts
- Cadastrar, editar e apagar hosts (`a`, `e`, `d`), com os campos: Alias, HostName/IP, User, Port,
  IdentityFile, Password e Notas.
- Validação: alias sem espaços e sem `* ? ! #`, sem aliases repetidos, porta numérica (vazia = 22).
- No campo IdentityFile vazio, um texto cinza mostra qual chave será usada (por exemplo
  `~/.ssh/id_ed25519.pub`).
- Importar do `~/.ssh/config` (`i`): lê Host, HostName, User, Port e IdentityFile, ignora curingas
  (`Host *`) e blocos `Match`, e pula aliases que já existem.
- Filtrar por alias, user ou IP (`/`).
- Trocar a senha mestra (`P`).

### Conexão
- **Workspace** (`Enter`): o ssh roda dentro do sshdeck. Os servidores ficam à esquerda
  (● = conectado), as informações da conexão embaixo (user, host, porta, chave, se tem senha, tempo
  conectado, notas) e o terminal no centro.
  - Várias sessões ao mesmo tempo: `Alt+←/→` troca entre elas e `x` fecha a do host selecionado.
  - `Ctrl+]` volta para a lista e a sessão continua rodando.
  - Histórico de 5000 linhas por sessão (`Shift+PgUp/PgDn`).
- **Tela cheia** (`s`): roda o ssh normal no terminal e volta para o sshdeck quando você sai.
- **ssh-copy-id** (`K`): instala sua chave no servidor e preenche a senha salva automaticamente.
  Sem IdentityFile, quem escolhe a chave é o `ssh-copy-id`: as do ssh-agent, se houver; senão o
  `~/.ssh/id*.pub` mais recente.

### RDP (Windows)
- No formulário, o campo **Tipo** alterna entre SSH e RDP (`←/→` ou espaço). Hosts RDP têm
  campos próprios: **Domínio**, **Resolução**, **Pasta compartilhada** e **Túnel SSH via**.
  O **User** é obrigatório e a porta padrão é 3389.
- `Enter` abre o RDP numa **janela própria** do FreeRDP (`xfreerdp3`, `sdl-freerdp3` ou
  `wlfreerdp3`, o primeiro instalado; dá para forçar com `SSHDECK_RDP_CLIENT`). Um terminal não
  consegue desenhar a área de trabalho do Windows, por isso ele não fica no painel central.
- A senha vai pelo **stdin** do FreeRDP (`/from-stdin`), nunca na linha de comando, onde
  apareceria no `ps`.
- **Resolução:** vazio = janela com 80% da tela e redimensionável; `full` = tela cheia;
  `1920x1080`; ou `90%`.
- **Pasta compartilhada:** aparece no Windows como a unidade `sshdeck`.
- **Clipboard** compartilhado entre Linux e Windows (`+clipboard`).
- **Certificado:** aceito na 1ª conexão e recusado se mudar depois (`/cert:tofu`, igual ao ssh).
- **Túnel SSH via `<alias>`:** para Windows que só é acessível passando por um servidor Linux.
  O sshdeck abre `ssh -N -L` por esse host SSH (preenchendo a senha dele), espera o túnel subir e
  conecta o RDP através dele. A chave do host do túnel precisa já ser conhecida: conecte nele
  uma vez por SSH antes.
- Enquanto a janela está aberta, o ícone do host fica verde. `x` fecha a janela (e o túnel).
  Se o FreeRDP falhar, o erro aparece na tela e os detalhes ficam no log.
- `s` e `K` são só para hosts SSH.

### Senhas
- `Alt+P` (dentro do terminal) **digita a senha direto na sessão**, sem passar pelo clipboard.
  Ideal para `sudo su`.
- `c`/`y` (e também o `Enter`) **copiam** a senha para o clipboard (Wayland/X11). Ela é apagada após
  **45s**, se o clipboard ainda contiver a senha.
- Se não houver clipboard do sistema, ele usa OSC 52: quem copia é o próprio terminal.

### Interface
- Os atalhos do contexto atual ficam sempre visíveis no rodapé, e as mensagens aparecem no canto
  da borda.
- `?` abre a ajuda completa.
- Hosts com senha salva levam o ícone de chave do Nerd Fonts.

## Teclas

| Tecla | Ação |
|---|---|
| `Enter` | conecta: SSH no painel central (ou vai para a sessão aberta), RDP em janela própria |
| `s` | conecta em tela cheia (ssh normal, volta ao sair) |
| `Tab` / `l` | foca o terminal |
| `x` | fecha a sessão (ou a janela RDP) do host selecionado |
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

No formulário: `Tab` navega, `←/→` no campo Tipo troca SSH/RDP, `Ctrl+S` salva, `Ctrl+R` mostra
a senha, `Ctrl+U` limpa o campo.

## Limitações conhecidas

- Com o cofre aberto, as senhas ficam em texto puro na memória do processo. A chave e o buffer
  descriptografado são zerados ao serem descartados, mas as senhas dentro da lista de hosts não.
- No `K`, a senha é passada ao `ssh-copy-id` por uma variável de ambiente (`SSHDECK_ASKPASS_PW`),
  só para esse processo. Outros usuários não conseguem lê-la, mas o seu próprio usuário e o root
  conseguem, enquanto o comando roda.
- No modo OSC 52, o sshdeck não consegue limpar o clipboard depois dos 45s.
- No túnel do RDP, a senha do host SSH também vai por variável de ambiente para o `ssh` em
  segundo plano, durante a conexão do túnel.
- Não há bloqueio automático por inatividade: o cofre fica aberto enquanto o programa estiver
  aberto.
