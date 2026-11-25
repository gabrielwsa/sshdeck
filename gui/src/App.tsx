import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  copyPassword,
  hostDelete,
  hostsImport,
  listHosts,
  markUsed,
  rdpCertSave,
  rdpTypePassword,
  updateInstall,
  setLang,
  sysinfoAllow,
  sysinfoCollect,
  tick,
  transferCancel,
  typePassword,
  vaultState,
  type Host,
  type TransferView,
  type VaultState,
} from "./api";
import { loadTranslations, t } from "./i18n";
import Lock from "./Lock";
import {
  Confirm,
  HelpModal,
  HostFormModal,
  LangModal,
  RekeyModal,
  TransferModal,
} from "./modals";
import Terminal, { type TermHandle } from "./Terminal";
import RdpTab, { type RdpEvent, type RdpHandle } from "./RdpTab";
import { UpdatePill, useUpdate } from "./Update";
import FilesTab from "./FilesTab";
import { TransferMenu } from "./TransferMenu";
import type { FileProto } from "./api";

/** Largura da barra lateral (px): padrão e limites do arraste. */
const SIDE_DEFAULT = 272;
const SIDE_MIN = 240;
const SIDE_MAX = 560;
import {
  ICON,
  KEY_ICON,
  RDP_ICON,
  SSH_ICON,
  ago,
  elapsed,
  hardwareText,
  kernelText,
  localDatetime,
} from "./ui";

export default function App() {
  const [state, setState] = useState<VaultState | null>(null);
  const [hosts, setHosts] = useState<Host[]>([]);

  const refresh = useCallback(async () => {
    const s = await vaultState();
    if (s === "unlocked") {
      // o idioma pode ter mudado com a escolha salva no cofre
      await loadTranslations();
      setHosts(await listHosts());
    }
    setState(s);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  if (state === null) return null;
  if (state !== "unlocked") return <Lock state={state} onOpen={refresh} />;
  return <Main hosts={hosts} setHosts={setHosts} />;
}

type Session = {
  key: number;
  alias: string;
  /** "copyid" é uma aba rodando o ssh-copy-id; ela fica aberta depois de terminar */
  kind: "ssh" | "copyid" | "rdp" | "files";
  /** aba "files": o protocolo do navegador de arquivos */
  proto?: FileProto;
  started: number;
  ended?: boolean;
  /** RDP ainda conectando (túnel, login) */
  connecting?: boolean;
};
type View = "host" | "session";
type ModalState =
  | { kind: "form"; editing: string | null }
  | { kind: "delete"; alias: string }
  | { kind: "sysinfo"; alias: string }
  | { kind: "cert"; alias: string; fingerprint: string }
  | { kind: "transfer"; alias: string }
  | { kind: "lang" }
  | { kind: "rekey" }
  | { kind: "help" }
  | { kind: "update" }
  | { kind: "quit" };

// campos de texto da janela (busca, formulários); o textarea interno do xterm não conta
const isTyping = (e: KeyboardEvent) =>
  e.target instanceof HTMLInputElement ||
  (e.target instanceof HTMLTextAreaElement &&
    !e.target.classList.contains("xterm-helper-textarea"));

function Main({ hosts, setHosts }: { hosts: Host[]; setHosts: (h: Host[]) => void }) {
  const [selected, setSelected] = useState<string | null>(hosts[0]?.alias ?? null);
  const [query, setQuery] = useState("");
  const [sessions, setSessions] = useState<Session[]>([]);
  const [active, setActive] = useState(0);
  const [view, setView] = useState<View>("host");
  const [termFocus, setTermFocus] = useState(false);
  const [showPanel, setShowPanel] = useState(() => localStorage.getItem("sshdeck.panel") !== "0");
  const [scrollback, setScrollback] = useState<Record<number, number>>({});
  const [status, setStatusState] = useState<{ msg: string; at: number } | null>(null);
  const [modal, setModal] = useState<ModalState | null>(null);
  const [transfers, setTransfers] = useState<TransferView[]>([]);
  const [collecting, setCollecting] = useState<string[]>([]);
  const [sideWidth, setSideWidth] = useState(() => {
    const saved = Number(localStorage.getItem("sidebar-width"));
    return saved >= SIDE_MIN && saved <= SIDE_MAX ? saved : SIDE_DEFAULT;
  });
  const [resizing, setResizing] = useState(false);
  /** menu "Transferir" aberto (as teclas vão para ele) */
  const [xferMenu, setXferMenu] = useState(false);
  const update = useUpdate();

  /** Reiniciar para atualizar: com sessões ou transferências abertas, pergunta antes. */
  function restartToUpdate() {
    if (openCount > 0 || transfers.length > 0) setModal({ kind: "update" });
    else void updateInstall();
  }

  /** Arrasta o divisor: a largura acompanha o mouse e fica salva para a próxima vez. */
  function startResize(e: React.PointerEvent<HTMLDivElement>) {
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    setResizing(true);
    const startX = e.clientX;
    const startW = sideWidth;
    let width = startW;
    const move = (ev: PointerEvent) => {
      width = Math.min(SIDE_MAX, Math.max(SIDE_MIN, startW + ev.clientX - startX));
      setSideWidth(width);
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      setResizing(false);
      localStorage.setItem("sidebar-width", String(width));
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  }
  const [, setNow] = useState(0);
  const terms = useRef<Record<number, TermHandle | RdpHandle | null>>({});
  const nextKey = useRef(0);
  const search = useRef<HTMLInputElement>(null);

  const setStatus = (msg: string) => setStatusState({ msg, at: Date.now() });
  const report = (e: unknown) => setStatus(String(e));

  // relógio de 1 s: tempo de conexão, "há 5 min" e o status que some depois de 6 s
  useEffect(() => {
    const timer = setInterval(() => {
      setNow((n) => n + 1);
      setStatusState((s) => (s && Date.now() - s.at > 6000 ? null : s));
    }, 1000);
    return () => clearInterval(timer);
  }, []);

  // RDP e rsync rodam no Rust: a janela pergunta o estado de tempos em tempos
  const tickRef = useRef<() => void>(() => {});
  tickRef.current = () => {
    tick()
      .then((r) => {
        setTransfers(r.transfers);
        for (const ev of r.events) setStatus(ev.msg);
      })
      .catch(() => {});
  };
  const busyBackground = transfers.length > 0;
  useEffect(() => {
    const timer = setInterval(() => tickRef.current(), busyBackground ? 250 : 1000);
    return () => clearInterval(timer);
  }, [busyBackground]);

  useEffect(() => localStorage.setItem("sshdeck.panel", showPanel ? "1" : "0"), [showPanel]);
  // o menu "Transferir" é do host selecionado: trocou de host, fecha
  useEffect(() => setXferMenu(false), [selected]);
  useEffect(() => {
    if (view === "session") setXferMenu(false);
  }, [view]);

  const visible = useMemo(() => {
    const q = query.toLowerCase();
    return hosts.filter(
      (h) =>
        !q ||
        h.alias.toLowerCase().includes(q) ||
        h.hostname.toLowerCase().includes(q) ||
        h.user.toLowerCase().includes(q),
    );
  }, [hosts, query]);

  // a seleção sempre aponta para um host visível
  const selIndex = Math.max(
    0,
    visible.findIndex((h) => h.alias === selected),
  );
  const current = visible[selIndex] as Host | undefined;
  const activeSession = sessions[active] as Session | undefined;
  /** aba da conexão do host (SSH ou RDP); as do ssh-copy-id e de arquivos não contam */
  const sessionOf = (alias: string) =>
    sessions.findIndex((s) => s.alias === alias && (s.kind === "ssh" || s.kind === "rdp"));
  /** true = conectado, false = RDP ainda conectando, undefined = sem conexão */
  const connection = (alias: string) => {
    const pos = sessionOf(alias);
    return pos < 0 ? undefined : !sessions[pos].connecting;
  };
  const hostByAlias = (alias: string) => hosts.find((h) => h.alias === alias);
  const inSession = view === "session" && !!activeSession;
  const openCount = sessions.filter((s) => s.kind !== "copyid").length;
  const fileTabs = sessions.filter((s) => s.kind === "files");

  useEffect(() => {
    if (sessions.length === 0) {
      setView("host");
      setTermFocus(false);
    }
    setActive((a) => Math.min(a, Math.max(sessions.length - 1, 0)));
  }, [sessions.length]);

  async function copy(h: Host) {
    try {
      setStatus(await copyPassword(h.alias));
    } catch (e) {
      report(e);
    }
  }

  function goToSession(pos: number) {
    setActive(pos);
    setView("session");
    setSelected(sessions[pos].alias);
    setTermFocus(true);
  }

  function addTab(alias: string, kind: Session["kind"], proto?: FileProto) {
    setSessions((ss) => [
      ...ss,
      {
        key: nextKey.current++,
        alias,
        kind,
        proto,
        started: Date.now(),
        connecting: kind === "rdp" || kind === "files",
      },
    ]);
    setActive(sessions.length);
    setView("session");
    setSelected(alias);
    setTermFocus(true);
  }

  function connect(h: Host) {
    const pos = sessionOf(h.alias);
    if (pos >= 0) return goToSession(pos);
    if (h.kind === "rdp") {
      // o RDP abre numa aba, como o SSH (IronRDP desenhando num canvas)
      if (!h.user) return setStatus(t("err.rdp_user"));
      setStatus(
        h.gateway
          ? t("st.rdp_opening_tunnel", { gw: h.gateway, alias: h.alias })
          : t("st.rdp_opening", { alias: h.alias }),
      );
      addTab(h.alias, "rdp");
      return;
    }
    if (h.hasPassword) void copy(h);
    addTab(h.alias, "ssh");
    markUsed(h.alias)
      .then(setHosts)
      .catch((e) => setStatus(t("st.save_error", { err: String(e) })));
    // 1ª conexão: pergunta se pode coletar as informações do servidor
    if (h.sysinfoAllowed === null && !modal) setModal({ kind: "sysinfo", alias: h.alias });
  }

  function copyId(h: Host) {
    if (h.kind !== "ssh") return setStatus(t("st.ssh_only"));
    addTab(h.alias, "copyid");
  }

  function collect(alias: string) {
    const h = hostByAlias(alias);
    if (!h || collecting.includes(alias)) return;
    if (h.kind !== "ssh") return setStatus(t("st.sysinfo_ssh_only"));
    setCollecting((c) => [...c, alias]);
    setStatus(t("st.sysinfo_started", { alias }));
    sysinfoCollect(alias)
      .then((list) => {
        setHosts(list);
        setStatus(t("st.sysinfo_ok", { alias }));
      }, report)
      .finally(() => setCollecting((c) => c.filter((a) => a !== alias)));
  }

  function openTransfer(h: Host) {
    if (h.kind !== "ssh") return setStatus(t("st.ssh_only"));
    if (transfers.some((x) => x.alias === h.alias))
      return setStatus(t("st.transfer_busy", { alias: h.alias }));
    setModal({ kind: "transfer", alias: h.alias });
  }

  /** rsync abre o popup; os demais protocolos abrem o navegador de arquivos. */
  function pickTransfer(h: Host, c: "rsync" | FileProto) {
    setXferMenu(false);
    if (c === "rsync") return openTransfer(h);
    const pos = sessions.findIndex((s) => s.alias === h.alias && s.kind === "files" && s.proto === c);
    if (pos >= 0) return goToSession(pos);
    setStatus(t("files.opening", { proto: c.toUpperCase(), alias: h.alias }));
    addTab(h.alias, "files", c);
  }

  function closeSession(alias: string) {
    const pos = sessionOf(alias);
    if (pos < 0) return;
    const rdp = sessions[pos].kind === "rdp";
    setSessions((ss) => ss.filter((s) => !(s.alias === alias && (s.kind === "ssh" || s.kind === "rdp"))));
    setView("host");
    setTermFocus(false);
    setStatus(rdp ? t("st.rdp_closed", { alias }) : t("st.session_closed", { alias }));
  }

  /** abas RDP já tiradas pelo app (aviso de certificado): o "fim" delas não vira mensagem */
  const rdpDropped = useRef(new Set<number>());

  /** O que a sessão RDP conta: conectou, certificado, fim. */
  function onRdpEvent(key: number, alias: string, ev: RdpEvent) {
    const drop = () => setSessions((ss) => ss.filter((x) => x.key !== key));
    if (ev.type === "connected") {
      setSessions((ss) => ss.map((x) => (x.key === key ? { ...x, connecting: false } : x)));
      setStatus(t("st.rdp_connected", { alias }));
      markUsed(alias).then(setHosts, () => {});
    } else if (ev.type === "certNew") {
      // 1ª conexão: guarda o certificado para comparar nas próximas
      rdpCertSave(alias, ev.fingerprint).then(setHosts, report);
    } else if (ev.type === "certChanged") {
      rdpDropped.current.add(key);
      drop();
      setModal({ kind: "cert", alias, fingerprint: ev.fingerprint });
    } else if (ev.type === "ended") {
      // a aba pode já ter saído pelo aviso de certificado
      if (rdpDropped.current.delete(key)) return;
      drop();
      setStatus(
        ev.error
          ? t("st.rdp_error", { alias, err: ev.error })
          : t("st.rdp_closed", { alias }),
      );
    }
  }

  function closeTab(key: number) {
    const s = sessions.find((x) => x.key === key);
    if (!s) return;
    if (s.kind === "ssh") return closeSession(s.alias);
    setSessions((ss) => ss.filter((x) => x.key !== key));
  }

  function sessionEnded(key: number) {
    const s = sessions.find((x) => x.key === key);
    if (!s) return;
    if (s.kind === "copyid") {
      // a aba fica aberta para mostrar o resultado; fecha no ×
      setSessions((ss) => ss.map((x) => (x.key === key ? { ...x, ended: true } : x)));
      return;
    }
    setSessions((ss) => ss.filter((x) => x.key !== key));
    setStatus(t("st.sessions_ended", { list: s.alias }));
  }

  function typePw(alias: string) {
    const pos = sessionOf(alias);
    const id = pos >= 0 ? terms.current[sessions[pos].key]?.id() : null;
    if (id == null) return;
    const typed = sessions[pos].kind === "rdp" ? rdpTypePassword(id, alias) : typePassword(id, alias);
    typed.then(setStatus, report);
    setTermFocus(true);
  }

  function requestQuit() {
    if (openCount || transfers.length) setModal({ kind: "quit" });
    else void getCurrentWindow().close();
  }

  /** Escolher um host: mostra a sessão dele se houver, senão os detalhes. */
  function select(alias: string) {
    setSelected(alias);
    const pos = sessionOf(alias);
    if (pos >= 0) {
      setActive(pos);
      setView("session");
    } else setView("host");
  }

  function move(delta: number) {
    const n = visible.length;
    if (n) select(visible[(selIndex + delta + n) % n].alias);
  }

  function importHosts() {
    hostsImport().then(({ added, hosts: list }) => {
      setHosts(list);
      setStatus(added ? t("st.imported", { n: added }) : t("st.import_none"));
    }, (e) => setStatus(t("st.import_error", { err: String(e) })));
  }

  async function pickLang(code: string) {
    setModal(null);
    try {
      const msg = await setLang(code);
      await loadTranslations();
      setHosts([...hosts]); // redesenha com os textos novos
      setStatus(msg);
    } catch (e) {
      report(e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (modal || xferMenu) return; // a janela modal (ou o menu) trata as próprias teclas
    const ctrl = e.ctrlKey,
      alt = e.altKey,
      shift = e.shiftKey,
      k0 = e.key;
    let handled = true;

    if (isTyping(e)) {
      return; // a busca trata as próprias teclas
    } else if (alt && e.code === "KeyI") {
      setShowPanel((p) => !p);
    } else if (inSession && termFocus && activeSession!.ended) {
      // aba do ssh-copy-id que já terminou: Enter/Esc fecham
      if (k0 === "Enter" || k0 === "Escape") closeTab(activeSession!.key);
      else if (ctrl && (e.code === "BracketRight" || k0 === "]")) setTermFocus(false);
      else handled = false;
    } else if (inSession && termFocus && activeSession!.kind === "files") {
      // o navegador de arquivos trata as próprias teclas; aqui só a volta e a troca de aba
      if (ctrl && (e.code === "BracketRight" || k0 === "]")) setTermFocus(false);
      else if (alt && e.key === "ArrowLeft") goToSession((active + sessions.length - 1) % sessions.length);
      else if (alt && e.key === "ArrowRight") goToSession((active + 1) % sessions.length);
      else handled = false;
    } else if (inSession && termFocus && activeSession!.kind === "rdp") {
      // no RDP quase tudo vai para o Windows; só a volta ao painel e o Alt+P ficam aqui
      if (ctrl && (e.code === "BracketRight" || k0 === "]")) setTermFocus(false);
      else if (alt && e.code === "KeyP") typePw(activeSession!.alias);
      else handled = false;
    } else if (inSession && termFocus) {
      if (ctrl && (e.code === "BracketRight" || e.key === "]" || e.key === "5")) setTermFocus(false);
      else if (alt && e.code === "KeyP") typePw(activeSession!.alias);
      else if (alt && e.key === "ArrowLeft") goToSession((active + sessions.length - 1) % sessions.length);
      else if (alt && e.key === "ArrowRight") goToSession((active + 1) % sessions.length);
      else if (shift && (e.key === "PageUp" || e.key === "PageDown"))
        (terms.current[activeSession!.key] as TermHandle | null)?.scroll(e.key === "PageUp" ? 1 : -1);
      else handled = false; // o resto vai para o ssh
    } else {
      const k = e.key;
      if (ctrl || alt) handled = false;
      else if (k === "q") requestQuit();
      else if (k === "Escape") {
        if (query) setQuery("");
        else requestQuit();
      } else if (k === "ArrowDown" || k === "j") move(1);
      else if (k === "ArrowUp" || k === "k") move(-1);
      else if ((k === "Home" || k === "g") && visible.length) select(visible[0].alias);
      else if ((k === "End" || k === "G") && visible.length) select(visible[visible.length - 1].alias);
      else if (k === "/") search.current?.focus();
      else if (k === "Tab" && sessions.length) goToSession(inSession ? active : 0);
      else if (k === "a") setModal({ kind: "form", editing: null });
      else if (k === "i") importHosts();
      else if (k === "L") setModal({ kind: "lang" });
      else if (k === "P") setModal({ kind: "rekey" });
      else if (k === "?") setModal({ kind: "help" });
      else if (!current) handled = false;
      else if (k === "Enter" || k === "s") connect(current);
      else if (k === "x") closeSession(current.alias);
      else if (k === "c" || k === "y") void copy(current);
      else if (k === "e") setModal({ kind: "form", editing: current.alias });
      else if (k === "d" || k === "Delete") setModal({ kind: "delete", alias: current.alias });
      else if (k === "K") copyId(current);
      else if (k === "r") openTransfer(current);
      else if (k === "f" && current.kind === "ssh") {
        setView("host");
        setXferMenu(true);
      }
      else if (k === "U") collect(current.alias);
      else handled = false;
    }

    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  // captura antes do xterm: os atalhos do sshdeck não chegam ao ssh
  const keyRef = useRef(onKey);
  keyRef.current = onKey;
  useEffect(() => {
    const listener = (e: KeyboardEvent) => keyRef.current(e);
    window.addEventListener("keydown", listener, true);
    return () => window.removeEventListener("keydown", listener, true);
  }, []);

  function onSearchKey(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape") {
      setQuery("");
      e.currentTarget.blur();
    } else if (e.key === "Enter") {
      e.currentTarget.blur();
      if (current) connect(current);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.currentTarget.blur();
      move(e.key === "ArrowDown" ? 1 : -1);
    } else return;
    e.preventDefault();
  }

  const hints: [string, string][] =
    inSession && termFocus && activeSession!.kind === "files"
      ? [
          ["Ctrl+]", t("h.panel")],
          ["Ctrl+D", t("gui.disconnect").toLowerCase()],
          ["Tab", t("files.h_side")],
          ["F5", t("files.h_copy")],
          ...(activeSession!.proto === "smb"
            ? [["Ctrl+C / Ctrl+V", t("files.h_clipboard")] as [string, string]]
            : []),
          ["F7", t("files.mkdir").toLowerCase()],
          ["F2", t("files.rename").toLowerCase()],
          ["Del", t("gui.delete").toLowerCase()],
          ["Ctrl+H", t("files.h_hidden")],
        ]
      : inSession && termFocus && activeSession!.kind === "rdp"
      ? [
          ["Ctrl+]", t("h.panel")],
          ["Alt+P", t("h.type_pw")],
        ]
      : inSession && termFocus
      ? [
          ["Ctrl+]", t("h.panel")],
          ["Alt+P", t("h.type_pw")],
          ["Alt+←/→", t("h.switch_session")],
          ["Alt+I", t("gui.panel").toLowerCase()],
        ]
      : [
          ["Enter", t("h.connect")],
          ["/", t("h.filter")],
          ["c", t("h.copy_pw")],
          ...(sessions.length ? ([["Tab", t("h.terminal")]] as [string, string][]) : []),
          ["?", t("gui.shortcuts").toLowerCase()],
        ];

  const panelHost = activeSession && hostByAlias(activeSession.alias);
  const modalHost = (alias: string) => hostByAlias(alias);

  return (
    <div className={`app${resizing ? " resizing" : ""}`}>
      <div className="body">
        <aside className="sidebar" style={{ width: sideWidth }}>
          <div className="brand">
            <span className="name">sshdeck</span>
            <span className="count">{t("gui.hosts_n", { n: hosts.length })}</span>
          </div>
          <label className="search">
            <span className="nf">{ICON.search}</span>
            <input
              ref={search}
              value={query}
              placeholder={t("gui.search")}
              spellCheck={false}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={onSearchKey}
            />
            {!query && <kbd>/</kbd>}
          </label>
          <div className="host-list">
            {visible.map((h) => (
              <HostItem
                key={h.alias}
                h={h}
                selected={h.alias === current?.alias}
                unfocused={termFocus}
                connection={connection(h.alias)}
                onClick={() => {
                  select(h.alias);
                  setTermFocus(false);
                }}
                onOpen={() => connect(h)}
              />
            ))}
            {visible.length === 0 && (
              <div className="empty-list">
                {hosts.length ? t("ui.no_match") : t("gui.select_host")}
              </div>
            )}
          </div>
          {transfers.length > 0 && (
            <div className="transfers">
              <h5>{t("gui.transfers")}</h5>
              {transfers.map((x) => (
                <TransferRow key={x.alias} x={x} onCancel={() => void transferCancel(x.alias)} />
              ))}
            </div>
          )}
          <div className="sidebar-foot">
            <button
              className="btn wide"
              title={`${t("gui.new_host")} (a)`}
              onClick={() => setModal({ kind: "form", editing: null })}
            >
              <span className="nf">{ICON.plus}</span> {t("gui.new_host")}
            </button>
            <button className="icon-btn" title={`${t("gui.import")} (i)`} onClick={importHosts}>
              {ICON.import}
            </button>
            <button
              className="icon-btn"
              title={`${t("gui.language")} (L)`}
              onClick={() => setModal({ kind: "lang" })}
            >
              {ICON.language}
            </button>
            <button
              className="icon-btn"
              title={`${t("gui.change_master")} (P)`}
              onClick={() => setModal({ kind: "rekey" })}
            >
              {ICON.key}
            </button>
            <button
              className="icon-btn"
              title={`${t("gui.shortcuts")} (?)`}
              onClick={() => setModal({ kind: "help" })}
            >
              {ICON.question}
            </button>
          </div>
        </aside>
        <div
          className="splitter"
          onPointerDown={startResize}
          onDoubleClick={() => {
            setSideWidth(SIDE_DEFAULT);
            localStorage.removeItem("sidebar-width");
          }}
        />

        <main className="main">
          {sessions.length > 0 && (
            <div className="tabs">
              {sessions.map((s, i) => {
                const h = hostByAlias(s.alias);
                const isActive = inSession && i === active;
                const copyid = s.kind === "copyid";
                const files = s.kind === "files";
                const proto = (s.proto ?? "").toUpperCase();
                return (
                  <div
                    key={s.key}
                    className={`tab${isActive ? " active" : ""}${isActive && termFocus ? " focused" : ""}${s.ended ? " ended" : ""}`}
                    title={copyid ? `ssh-copy-id ${s.alias}` : files ? `${proto} ${s.alias}` : `ssh ${s.alias}`}
                    onMouseDown={() => goToSession(i)}
                  >
                    <span className={`nf${copyid ? " copyid" : ""}${s.connecting ? " wait" : ""}`}>
                      {copyid ? ICON.key : files ? ICON.folder : h?.kind === "rdp" ? RDP_ICON : SSH_ICON}
                    </span>
                    <span className="label">
                      {copyid ? `ssh-copy-id ${s.alias}` : files ? `${s.alias} · ${proto}` : s.alias}
                    </span>
                    <button
                      className="icon-btn close"
                      title={`${t("gui.close")} (x)`}
                      onMouseDown={(e) => e.stopPropagation()}
                      onClick={() => closeTab(s.key)}
                    >
                      {ICON.close}
                    </button>
                  </div>
                );
              })}
              <div className="spacer" />
              {inSession && (scrollback[activeSession!.key] ?? 0) > 0 && (
                <span className="history-tag">
                  {t("ui.history", { n: scrollback[activeSession!.key] })}
                </span>
              )}
              {inSession && (
                <div className="tools">
                  <button
                    className={`icon-btn${showPanel ? " on" : ""}`}
                    title={`${t("gui.panel")} (Alt+I)`}
                    onClick={() => setShowPanel((p) => !p)}
                  >
                    {ICON.panel}
                  </button>
                </div>
              )}
            </div>
          )}

          <div className="content">
            {/* os terminais ficam montados mesmo escondidos, para as sessões não caírem */}
            <div className="term-area" style={{ display: inSession ? "block" : "none" }}>
              {sessions.map((s, i) =>
                s.kind === "files" ? (
                  <FilesTab
                    key={s.key}
                    alias={s.alias}
                    proto={s.proto!}
                    visible={inSession && i === active}
                    focused={termFocus && !modal}
                    onStatus={setStatus}
                    onReady={() => {
                      setSessions((ss) => ss.map((x) => (x.key === s.key ? { ...x, connecting: false } : x)));
                      setStatus(t("files.connected", { proto: s.proto!.toUpperCase(), alias: s.alias }));
                    }}
                    onFail={(err) => {
                      setSessions((ss) => ss.filter((x) => x.key !== s.key));
                      setStatus(t("files.open_failed", { proto: s.proto!.toUpperCase(), alias: s.alias, err }));
                    }}
                    onDisconnect={() => {
                      // fecha a aba e volta para a tela do host
                      setSessions((ss) => ss.filter((x) => x.key !== s.key));
                      select(s.alias);
                      setView("host");
                      setTermFocus(false);
                      setStatus(t("files.disconnected", { proto: s.proto!.toUpperCase(), alias: s.alias }));
                    }}
                    onClick={() => setTermFocus(true)}
                  />
                ) : s.kind === "rdp" ? (
                  <RdpTab
                    key={s.key}
                    ref={(h) => {
                      terms.current[s.key] = h;
                    }}
                    alias={s.alias}
                    resolution={hostByAlias(s.alias)?.resolution ?? ""}
                    visible={inSession && i === active}
                    focused={termFocus && !modal}
                    onEvent={(ev) => onRdpEvent(s.key, s.alias, ev)}
                    onClick={() => setTermFocus(true)}
                  />
                ) : (
                  <Terminal
                    key={s.key}
                    ref={(h) => {
                      terms.current[s.key] = h;
                    }}
                    alias={s.alias}
                    kind={s.kind}
                    visible={inSession && i === active}
                    focused={termFocus && !modal}
                    onExit={() => sessionEnded(s.key)}
                    onScrollback={(n) => setScrollback((m) => ({ ...m, [s.key]: n }))}
                    onClick={() => setTermFocus(true)}
                  />
                ),
              )}
              {inSession && activeSession.connecting && activeSession.kind === "rdp" && (
                <div className="rdp-wait">
                  <span className="nf spin">{ICON.spinner}</span> {t("info.connecting")}
                </div>
              )}
            </div>
            {inSession && showPanel && panelHost && (activeSession.kind === "ssh" || activeSession.kind === "rdp") && (
              <SessionPanel
                h={panelHost}
                session={activeSession}
                onTypePw={() => typePw(panelHost.alias)}
                onCopy={() => copy(panelHost)}
                onSmb={() => pickTransfer(panelHost, "smb")}
                onDisconnect={() => closeSession(panelHost.alias)}
              />
            )}
            {!inSession &&
              (current ? (
                <HostDetail
                  h={current}
                  connection={connection(current.alias)}
                  collecting={collecting.includes(current.alias)}
                  transfer={transfers.find((x) => x.alias === current.alias)}
                  onConnect={() => connect(current)}
                  onDisconnect={() => closeSession(current.alias)}
                  onCopy={() => copy(current)}
                  onCopyId={() => copyId(current)}
                  xferMenu={xferMenu}
                  onXferMenu={setXferMenu}
                  onPickTransfer={(c) => pickTransfer(current, c)}
                  onCollect={() => collect(current.alias)}
                  onEdit={() => setModal({ kind: "form", editing: current.alias })}
                  onDelete={() => setModal({ kind: "delete", alias: current.alias })}
                />
              ) : (
                <div className="placeholder">
                  <div className="empty-state">
                    <p>{hosts.length ? t("gui.select_host") : t("ui.empty").split("\n")[0]}</p>
                    {!hosts.length && (
                      <div className="actions">
                        <button
                          className="btn primary"
                          onClick={() => setModal({ kind: "form", editing: null })}
                        >
                          <span className="nf">{ICON.plus}</span> {t("gui.new_host")}
                        </button>
                        <button className="btn" onClick={importHosts}>
                          <span className="nf">{ICON.import}</span> {t("gui.import")}
                        </button>
                      </div>
                    )}
                  </div>
                </div>
              ))}
          </div>
        </main>
      </div>

      <footer className="statusbar">
        {status ? (
          <span className="msg">
            <span className="nf">{ICON.info}</span>
            {status.msg}
          </span>
        ) : (
          <span>
            {t("gui.hosts_n", { n: hosts.length })}
            {openCount > 0 && ` · ${t("gui.sessions_n", { n: openCount })}`}
          </span>
        )}
        <span className="right">
          <UpdatePill s={update} onRestart={restartToUpdate} />
          {hints.map(([k, d]) => (
            <span className="hint" key={k}>
              <kbd>{k}</kbd> {d}
            </span>
          ))}
        </span>
      </footer>

      {modal?.kind === "quit" && (
        <Confirm
          title={t("ui.quit_title")}
          text={[
            openCount ? t("ui.quit_q", { n: openCount }) : "",
            transfers.length ? t("ui.quit_transfers_q", { n: transfers.length }) : "",
          ]
            .filter(Boolean)
            .join("\n")}
          yes={t("gui.quit")}
          danger
          onYes={() => void getCurrentWindow().close()}
          onNo={() => setModal(null)}
        />
      )}
      {modal?.kind === "form" && (
        <HostFormModal
          editing={modal.editing}
          hosts={hosts}
          onClose={() => setModal(null)}
          onSaved={(list, alias) => {
            setHosts(list);
            setModal(null);
            setQuery("");
            setSelected(alias);
            setView("host");
            setStatus(t("st.saved", { alias }));
          }}
        />
      )}
      {modal?.kind === "delete" && (
        <Confirm
          title={t("ui.delete_title")}
          text={t("ui.delete_q", { alias: modal.alias })}
          yes={t("gui.delete")}
          danger
          onNo={() => setModal(null)}
          onYes={() => {
            const alias = modal.alias;
            setModal(null);
            hostDelete(alias).then((list) => {
              setHosts(list);
              setStatus(t("st.removed", { alias }));
            }, report);
          }}
        />
      )}
      {modal?.kind === "sysinfo" && (
        <Confirm
          title={t("ui.sysinfo_title")}
          text={t("ui.sysinfo_q", { alias: modal.alias })}
          yes={t("gui.collect_yes")}
          no={t("gui.not_now")}
          onNo={() => {
            sysinfoAllow(modal.alias, false).then(setHosts, report);
            setModal(null);
          }}
          onYes={() => {
            const alias = modal.alias;
            setModal(null);
            sysinfoAllow(alias, true).then(setHosts, report);
            // espera a sessão aceitar a chave do servidor e a senha funcionar
            setTimeout(() => collect(alias), 5000);
          }}
        />
      )}
      {modal?.kind === "cert" && (
        <Confirm
          title={t("ui.cert_title")}
          text={t("gui.cert_q", { alias: modal.alias, thumb: modal.fingerprint })}
          yes={t("gui.trust")}
          no={t("gui.refuse")}
          onNo={() => {
            setStatus(t("st.cert_kept", { alias: modal.alias }));
            setModal(null);
          }}
          onYes={() => {
            const { alias, fingerprint } = modal;
            setModal(null);
            rdpCertSave(alias, fingerprint).then((list) => {
              setHosts(list);
              const h = list.find((x) => x.alias === alias);
              if (h) connect(h);
            }, report);
          }}
        />
      )}
      {modal?.kind === "transfer" && modalHost(modal.alias) && (
        <TransferModal
          host={modalHost(modal.alias)!}
          onClose={() => setModal(null)}
          onStarted={(msg) => {
            setModal(null);
            setStatus(msg);
            tickRef.current();
          }}
        />
      )}
      {modal?.kind === "lang" && <LangModal onPick={pickLang} onClose={() => setModal(null)} />}
      {modal?.kind === "rekey" && (
        <RekeyModal
          onClose={() => setModal(null)}
          onDone={(msg) => {
            setModal(null);
            setStatus(msg);
          }}
        />
      )}
      {modal?.kind === "help" && <HelpModal update={update} onClose={() => setModal(null)} />}
      {modal?.kind === "update" && (
        <Confirm
          title={t("gui.upd_title")}
          text={t("gui.upd_restart_q")}
          yes={t("gui.upd_restart")}
          onNo={() => setModal(null)}
          onYes={() => {
            setModal(null);
            void updateInstall();
          }}
        />
      )}
    </div>
  );
}

function TransferRow({ x, onCancel }: { x: TransferView; onCancel: () => void }) {
  return (
    <div className="xfer">
      <span className="name">
        <span className="nf">{x.upload ? ICON.upload : ICON.download}</span> {x.alias}
      </span>
      <button className="icon-btn" title={t("gui.cancel")} onClick={onCancel}>
        {ICON.close}
      </button>
      <span className="nums">
        {x.bytes ? `${x.percent}% · ${x.bytes} · ${x.rate} · ${x.eta}` : t("ui.transfer_preparing")}
      </span>
      <div className="bar">
        <div style={{ width: `${x.percent}%` }} />
      </div>
    </div>
  );
}

/** `connection`: true = conectado (verde), false = conectando (amarelo), undefined = nada. */
function KindBadge({ h, connection, big }: { h: Host; connection?: boolean; big?: boolean }) {
  return (
    <span className={`badge-icon${h.kind === "rdp" ? " rdp" : ""}${big ? " big" : ""}`}>
      {h.kind === "rdp" ? RDP_ICON : SSH_ICON}
      {connection !== undefined && <span className={`dot${connection ? "" : " wait"}`} />}
    </span>
  );
}

function HostItem({
  h,
  selected,
  unfocused,
  connection,
  onClick,
  onOpen,
}: {
  h: Host;
  selected: boolean;
  unfocused: boolean;
  connection?: boolean;
  onClick: () => void;
  onOpen: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (selected) ref.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  return (
    <div
      ref={ref}
      className={`host-item${selected ? " sel" : ""}${selected && unfocused ? " unfocused" : ""}`}
      onMouseDown={onClick}
      onDoubleClick={onOpen}
    >
      <KindBadge h={h} connection={connection} />
      <span className="alias">{h.alias}</span>
      {h.hasPassword && <span className="lock">{KEY_ICON}</span>}
      <span className="addr">
        {h.user ? `${h.user}@` : ""}
        {h.hostname}
      </span>
    </div>
  );
}

type Row = [string, React.ReactNode, boolean?];

function KV({ rows }: { rows: Row[] }) {
  return (
    <div className="kv">
      {rows.map(([k, v, plain]) => (
        <div key={k} style={{ display: "contents" }}>
          <span className="k">{k}</span>
          <span className={`v${plain ? " plain" : ""}`}>{v}</span>
        </div>
      ))}
    </div>
  );
}

const faint = (text: string) => <span className="faint">{text}</span>;

function accessRows(h: Host): Row[] {
  const rows: Row[] = [
    [t("info.user"), h.user || faint("-")],
    [t("info.host"), h.hostname],
    [t("info.port"), String(h.port)],
  ];
  if (h.kind === "ssh") rows.push([t("info.key"), h.identityFile || faint(t("info.key_default"))]);
  else {
    if (h.domain) rows.push([t("field.domain"), h.domain]);
    rows.push([t("info.resolution"), h.resolution || faint(t("gui.res_default"))]);
    if (h.gateway) rows.push([t("field.gateway"), h.gateway]);
  }
  rows.push([
    t("info.password"),
    h.hasPassword ? (
      <span>
        <span className="nf green">{KEY_ICON}</span>&nbsp; {t("gui.password_saved")}
      </span>
    ) : (
      faint(t("gui.no_password"))
    ),
    true,
  ]);
  return rows;
}

function serverRows(h: Host): Row[] | null {
  const i = h.sysinfo;
  if (!i) return null;
  return [
    [t("info.system"), i.os || faint("-"), true],
    [t("info.hostname"), i.hostname || faint("-")],
    [t("info.kernel"), kernelText(i)],
    [t("info.hardware"), hardwareText(i) || faint("-")],
    [t("info.collected"), localDatetime(i.collected)],
  ];
}

function HostDetail({
  h,
  connection,
  collecting,
  transfer,
  onConnect,
  onDisconnect,
  onCopy,
  onCopyId,
  xferMenu,
  onXferMenu,
  onPickTransfer,
  onCollect,
  onEdit,
  onDelete,
}: {
  h: Host;
  connection?: boolean;
  collecting: boolean;
  transfer?: TransferView;
  onConnect: () => void;
  onDisconnect: () => void;
  onCopy: () => void;
  onCopyId: () => void;
  xferMenu: boolean;
  onXferMenu: (open: boolean) => void;
  onPickTransfer: (c: "rsync" | FileProto) => void;
  onCollect: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const connected = connection !== undefined;
  const ssh = h.kind === "ssh";
  const server = serverRows(h);
  return (
    <div className="detail">
      <div className="detail-inner">
        <div className="detail-head">
          <KindBadge h={h} connection={connection} big />
          <div>
            <h1>
              {h.alias}
              <span className={`kind-tag${ssh ? "" : " rdp"}`}>{ssh ? "SSH" : "RDP"}</span>
              {connection === true && <span className="conn-tag">● {t("info.connected")}</span>}
              {connection === false && (
                <span className="conn-tag wait">● {t("info.connecting")}</span>
              )}
            </h1>
            <div className="sub">
              {h.user ? `${h.user}@` : ""}
              {h.hostname}:{h.port}
            </div>
          </div>
        </div>

        <div className="actions">
          <button className="btn primary" title={`${t("gui.connect")} (Enter)`} onClick={onConnect}>
            <span className="nf">{ICON.plug}</span>
            {connected ? t("gui.open_session") : t("gui.connect")}
          </button>
          {connected && (
            <button className="btn danger" title={`${t("gui.disconnect")} (x)`} onClick={onDisconnect}>
              <span className="nf">{ICON.power}</span> {t("gui.disconnect")}
            </button>
          )}
          <button
            className="btn"
            title={`${t("gui.copy_pw")} (c)`}
            disabled={!h.hasPassword}
            onClick={onCopy}
          >
            <span className="nf">{ICON.copy}</span> {t("gui.copy_pw")}
          </button>
          {ssh ? (
            <>
              <button className="btn" title="ssh-copy-id (K)" onClick={onCopyId}>
                <span className="nf">{ICON.key}</span> ssh-copy-id
              </button>
              <TransferMenu
                alias={h.alias}
                open={xferMenu}
                rsyncBusy={!!transfer}
                onOpen={() => onXferMenu(true)}
                onClose={() => onXferMenu(false)}
                onPick={onPickTransfer}
              />
              <button
                className="btn"
                title={`${t("gui.collect")} (Shift+U)`}
                disabled={collecting}
                onClick={onCollect}
              >
                <span className={`nf${collecting ? " spin" : ""}`}>
                  {collecting ? ICON.spinner : ICON.refresh}
                </span>{" "}
                {collecting ? t("gui.collecting") : t("gui.collect")}
              </button>
            </>
          ) : (
            <SmbButton onClick={() => onPickTransfer("smb")} />
          )}
          <span className="sep" />
          <button className="btn ghost" title={`${t("gui.edit")} (e)`} onClick={onEdit}>
            <span className="nf">{ICON.edit}</span> {t("gui.edit")}
          </button>
          <button className="btn ghost danger" title={`${t("gui.delete")} (d)`} onClick={onDelete}>
            <span className="nf">{ICON.trash}</span> {t("gui.delete")}
          </button>
        </div>
        {transfer && (
          <div className="detail-progress xfer">
            <span className="name">
              rsync {transfer.upload ? t("transfer.up") : t("transfer.down")}
            </span>
            <span className="nums">
              {transfer.bytes
                ? `${transfer.percent}% · ${transfer.bytes} · ${transfer.rate} · ${transfer.eta}`
                : t("ui.transfer_preparing")}
            </span>
            <div className="bar">
              <div style={{ width: `${transfer.percent}%` }} />
            </div>
          </div>
        )}

        <div className="sections">
          <section className="section">
            <h2>{t("gui.access")}</h2>
            <KV rows={accessRows(h)} />
          </section>
          {ssh && (
            <section className="section">
              <h2>{t("gui.server")}</h2>
              {server ? (
                <KV rows={server} />
              ) : (
                <div className="empty-section">
                  {collecting ? t("gui.collecting") : t("gui.not_collected")}
                  {!collecting && (
                    <button className="btn" title="Shift+U" onClick={onCollect}>
                      <span className="nf">{ICON.refresh}</span> {t("gui.collect")}
                    </button>
                  )}
                </div>
              )}
            </section>
          )}
          <section className="section">
            <h2>{t("gui.activity")}</h2>
            <KV
              rows={[
                [t("info.last"), h.lastUsed ? ago(h.lastUsed) : faint(t("info.never")), true],
                [t("info.created"), h.created ? localDatetime(h.created) : faint("-")],
                [t("info.updated"), h.updated ? localDatetime(h.updated) : faint(t("info.never"))],
              ]}
            />
          </section>
          <section className="section">
            <h2>{t("field.notes")}</h2>
            {h.notes ? <div className="notes">{h.notes}</div> : <div className="faint">{t("gui.no_notes")}</div>}
          </section>
          {ssh && (
            <section className="section wide">
              <h2>
                {t("gui.ssh_config")}
                <span className="h2-note mono">~/.ssh/sshdeck.conf</span>
              </h2>
              <pre className="code">{sshConfigBlock(h)}</pre>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}

/** O bloco que o sshdeck gera para o host no ~/.ssh/sshdeck.conf. */
function sshConfigBlock(h: Host) {
  const lines = [`Host ${h.alias}`, `    HostName ${h.hostname}`];
  if (h.user) lines.push(`    User ${h.user}`);
  lines.push(`    Port ${h.port}`);
  if (h.identityFile) lines.push(`    IdentityFile ${h.identityFile}`);
  return lines.join("\n");
}

function SmbButton({ onClick }: { onClick: () => void }) {
  return (
    <button className="btn" onClick={onClick}>
      <span className="nf">{ICON.folder}</span> {t("files.smb")}
    </button>
  );
}

function SessionPanel({
  h,
  session,
  onTypePw,
  onCopy,
  onSmb,
  onDisconnect,
}: {
  h: Host;
  session: Session;
  onTypePw: () => void;
  onCopy: () => void;
  onSmb: () => void;
  onDisconnect: () => void;
}) {
  const server = serverRows(h);
  return (
    <aside className="info-panel">
      <div className="head">
        <KindBadge h={h} connection />
        <div>
          <div className="name">{h.alias}</div>
          <div className="time">
            {t("gui.connected_for", {
              t: elapsed(Math.floor((Date.now() - session.started) / 1000)),
            })}
          </div>
        </div>
      </div>

      <div className="buttons">
        <button className="btn" disabled={!h.hasPassword} onClick={onTypePw}>
          <span className="nf">{ICON.keyboard}</span> {t("gui.type_pw")} <kbd>Alt+P</kbd>
        </button>
        <button className="btn" disabled={!h.hasPassword} onClick={onCopy}>
          <span className="nf">{ICON.copy}</span> {t("gui.copy_pw")}
        </button>
        {h.kind === "rdp" && <SmbButton onClick={onSmb} />}
        <button className="btn danger" onClick={onDisconnect}>
          <span className="nf">{ICON.power}</span> {t("gui.disconnect")}
        </button>
      </div>

      <section className="section">
        <h2>{t("gui.access")}</h2>
        <KV rows={accessRows(h)} />
      </section>
      {server && (
        <section className="section">
          <h2>{t("gui.server")}</h2>
          <KV rows={server} />
        </section>
      )}
      {h.notes && (
        <section className="section">
          <h2>{t("field.notes")}</h2>
          <div className="notes">{h.notes}</div>
        </section>
      )}
    </aside>
  );
}
