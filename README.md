# sshdeck

Gerenciador de conexões SSH e RDP para Linux (Tauri + React, com o motor em Rust): cadastra hosts
(alias, IP, user, porta, chave), gera o `~/.ssh/config` e guarda as senhas num cofre criptografado
para usar no servidor (`sudo su` etc.). SSH e área de trabalho do Windows (RDP) abrem em abas,
dentro do app.

## Instalar

Baixe da [página de releases](../../releases/latest) o arquivo do seu sistema:

| Sistema | Arquivo | Instalar |
|---|---|---|
| Arch | `sshdeck-X-1-x86_64.pkg.tar.zst` | `sudo pacman -U sshdeck-*.pkg.tar.zst` |
| Debian, Ubuntu, Mint… | `sshdeck_X_amd64.deb` | `sudo apt install ./sshdeck_*.deb` |
| Fedora, openSUSE… | `sshdeck-X-1.x86_64.rpm` | `sudo dnf install ./sshdeck-*.rpm` |
| Qualquer distro | `sshdeck_X_amd64.AppImage` | `chmod +x` e abrir |

Os pacotes instalam o `sshdeck-gui`, com atalho no menu. A fonte do terminal e dos ícones
([JetBrainsMono Nerd Font](https://www.nerdfonts.com/), licença OFL 1.1 em
`gui/src/assets/fonts/OFL.txt`) vai dentro do app: não precisa instalar nada.

## Atualizações

O app procura uma versão nova ao abrir e a cada 4 horas; quando sai uma, aparece um aviso na barra
de status (`Atualizar para X` → `Baixando… N%` → `Reiniciar e atualizar`):

- **AppImage:** baixa, troca o arquivo e abre de novo, sem pedir senha;
- **pacote (pacman, apt, dnf, zypper):** baixa e instala com `pkexec`, que pede a senha do
  sistema, e abre de novo;
- **compilado à mão:** só avisa e abre a página do release.

Antes de instalar, o arquivo baixado é conferido com o `SHA256SUMS` do release. Com sessões
abertas, o app pergunta antes de reiniciar. `?` mostra a versão instalada e tem o botão
**Buscar atualizações**.

## Funcionalidades

Visual escuro (estilo Termius / VS Code): hosts na barra lateral (redimensionável) com busca,
detalhes do host selecionado no centro, sessões em abas e um painel de informações recolhível à
direita (`Alt+I`). Tudo tem botão e atalho de teclado (`?` mostra a lista).

### Hosts
- Cadastrar, editar e apagar (`a`, `e`, `d`): Alias, HostName/IP, User, Port, IdentityFile,
  Password e Notas. `Ctrl+S` salva.
- Validação: alias sem espaços e sem `* ? ! #`, sem aliases repetidos, porta numérica.
- No IdentityFile vazio, um texto cinza mostra qual chave será usada.
- Importar do `~/.ssh/config` (`i`): lê Host, HostName, User, Port e IdentityFile, ignora curingas
  e blocos `Match`, e pula aliases que já existem.
- Buscar por alias, user ou IP (`/`).
- Trocar a senha mestra (`P`) e o idioma (`L`).

### SSH
- `Enter` abre o ssh numa aba (xterm.js). Várias sessões ao mesmo tempo: `Alt+←/→` troca,
  `x` fecha a do host selecionado, `Ctrl+]` volta para a lista com a sessão rodando.
- `Alt+P` **digita a senha salva** na sessão, sem passar pelo clipboard. Ideal para `sudo su`.
- `c` copia a senha; ela some do clipboard depois de 45 s.
- **Informações do servidor** (`Shift+U`): uma segunda conexão rápida lê `/etc/os-release`,
  `uname`, `nproc` e `/proc/meminfo`. O resultado fica no cofre. Só coleta quando você pede; na
  1ª conexão a um host, o app pergunta se pode.
- **ssh-copy-id** (`K`) numa aba, com a senha salva preenchida. Sem IdentityFile, quem escolhe a
  chave é o `ssh-copy-id`: as do ssh-agent, se houver; senão a `~/.ssh/id*.pub` mais recente.
- **rsync** (`r`): baixa ou envia arquivos em segundo plano, com autocomplete dos caminhos (no
  servidor e no local) e progresso na barra lateral. Roda `rsync -azh --partial`: se a conexão
  cair, rodar de novo continua de onde parou. O servidor precisa ter `rsync`.

### Arquivos (SFTP e FTP)
- O botão **Transferir** (`f`) abre um menu com **rsync** (o popup), **SFTP** e **FTP**. Ao abrir,
  o app testa as portas e desabilita o que não responde. Portas diferentes do padrão (SFTP = a do
  SSH, FTP = 21) ficam em **Porta SFTP** / **Porta FTP** no cadastro do host.
- SFTP e FTP abrem um **navegador de arquivos em aba**, em duas colunas: este computador à esquerda
  e o servidor à direita. Usa o user e a senha salvos do host; o SFTP passa pelo `ssh` do sistema
  (valem o `~/.ssh/config`, as chaves e o known_hosts).
- Copiar: arrastar entre as colunas, duplo clique num arquivo, os botões do meio ou `F5`. Pastas vão
  inteiras, com progresso embaixo e botão de cancelar; antes de sobrescrever, o app pergunta.
- `Enter` abre a pasta, `Backspace` sobe, `Tab` troca de coluna, `Espaço`/`Ctrl+clique` seleciona
  vários, `F7` nova pasta, `F2` renomeia, `Del` apaga (pastas com tudo dentro, depois de
  confirmar), `Ctrl+H` mostra os ocultos, `Ctrl+L` digita o caminho, `Ctrl+R` atualiza.
- **Pastas grandes:** cada coluna carrega no máximo 500 itens por vez (pastas primeiro), com
  **Carregar mais** no fim; o limite (200, 500, 1000 ou todos) fica no rodapé e é salvo. O filtro
  por nome (`Ctrl+F`) procura na pasta inteira, não só no que já foi carregado.
- O FTP não tem criptografia (o protocolo não tem); a conexão da aba se refaz sozinha se o
  servidor derrubar por inatividade.

### RDP (Windows)
- A área de trabalho abre numa aba, desenhada pelo próprio app (IronRDP); não precisa de
  FreeRDP. Mouse, roda, teclado (pelas teclas físicas, no layout configurado no Windows) e
  `Alt+P` para digitar a senha.
- Login por NLA (CredSSP): a senha só sai depois de conferir o certificado do Windows.
- **Certificado:** a impressão digital é guardada no cofre na 1ª conexão. Se mudar (o Windows
  renova o dele a cada ~6 meses), o app mostra a nova e pergunta se você confia, antes de enviar
  a senha.
- **Resolução:** vazio = acompanha o tamanho da aba (o Windows se ajusta ao redimensionar);
  `1920x1080` = fixa.
- **Túnel SSH via `<alias>`:** para Windows acessível só passando por um servidor Linux. O app
  abre `ssh -N -L` por esse host (com a senha dele) e conecta o RDP através do túnel.
- Se a tela vier com defeito num servidor, `SSHDECK_RDP_CODECS=remotefx:off` desliga a
  compressão RemoteFX.
- **Área de transferência compartilhada (texto):** o que você copia no Windows vai para o
  clipboard do Linux, e o que copia no Linux fica disponível para colar no Windows (conferido a
  cada segundo e ao voltar para a aba). Senhas copiadas pelo botão **Copiar senha** do sshdeck
  não vão para o Windows, para não ficarem no histórico de clipboard dele (`Win+V`).

## Idiomas

Português, inglês e espanhol. Ordem de escolha: variável `SSHDECK_LANG` (`pt`, `en`, `es`); a
escolha feita com `L` (salva no cofre); o idioma do sistema; inglês. A tela de desbloqueio usa o
idioma do sistema, porque a escolha salva fica dentro do cofre. As traduções ficam em
`locales/app.yml`.

## Onde ficam as coisas

| O quê | Onde | Permissão | Criptografado? |
|---|---|---|---|
| **Cofre** (hosts, senhas, notas, certificados RDP aceitos) | `~/.local/share/sshdeck/vault.bin` (ou `$XDG_DATA_HOME/sshdeck/`) | pasta `700`, arquivo `600` | **Sim** |
| Config SSH gerada | `~/.ssh/sshdeck.conf` | `600` | Não, mas **não tem senhas**: só Host, HostName, User, Port e IdentityFile |
| Seu config SSH | `~/.ssh/config`: o sshdeck só coloca `Include ~/.ssh/sshdeck.conf` na 1ª linha | `600` | Não |
| Backup do seu config | `~/.ssh/config.sshdeck.bak`, criado uma vez, antes da 1ª alteração | | Não |

O cofre é um único arquivo, criptografado inteiro: nem nomes e IPs dos servidores ficam legíveis
nele, só no `sshdeck.conf`, que o ssh precisa ler. Com o `Include` no topo, `ssh <alias>` funciona
em qualquer lugar, até fora do sshdeck.

## Criptografia do cofre

- **Derivação da chave:** Argon2id (19 MiB, 2 passadas, 1 thread) transforma a senha mestra numa
  chave de 256 bits. Salt aleatório de 16 bytes, renovado ao trocar a senha mestra.
- **Cifra:** XChaCha20-Poly1305, com nonce aleatório novo de 24 bytes a cada gravação. Arquivo
  alterado ou senha errada fazem a abertura falhar; o cofre nunca abre corrompido.
- **Formato:** `SSHDK1` (6 bytes) | salt (16) | nonce (24) | JSON dos hosts criptografado + tag (16).
- **Gravação segura:** escreve em `vault.tmp`, força a gravação no disco e só então renomeia.
- **Senha mestra:** mínimo de 4 caracteres. **Não existe recuperação.**

## Desenvolver

```sh
cd gui
npm install
npx tauri dev      # abre a janela com recarga ao vivo
npx tauri build    # gera .deb, .rpm e AppImage em target/release/bundle/
```

Precisa de Rust, Node e `webkit2gtk-4.1` (`pacman -S webkit2gtk-4.1`). O código fica em:

- `core/`: cofre, ssh config, rsync, túnel, askpass e idioma (sem interface);
- `gui/src-tauri/`: comandos do app, terminal (pty), RDP (IronRDP) e atualizador;
- `gui/src/`: a janela (React).

## Lançar uma versão

O workflow `.github/workflows/release.yml` compila no GitHub e publica o release com todos os
arquivos acima. A versão vem da tag:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

(ou Actions → Release → Run workflow, informando a versão). Versões com `-` (`1.0.0-rc.1`) saem
como pré-lançamento e o atualizador do app não as oferece.

## Limitações conhecidas

- Com o cofre aberto, as senhas ficam em texto puro na memória do processo.
- No `K`, no rsync, na coleta de informações e no túnel do RDP, a senha do host vai por variável
  de ambiente (`SSHDECK_ASKPASS_PW`) para o `ssh` em segundo plano, enquanto ele roda. Outros
  usuários não a leem, mas o seu usuário e o root sim.
- RDP: a área de transferência compartilha só texto (imagens e arquivos não); sem som e sem
  pastas compartilhadas, por enquanto.
- Fechar o app cancela os rsync em andamento (ele pergunta antes).
- Não há bloqueio automático por inatividade.
