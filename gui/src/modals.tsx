import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  hostGet,
  hostSave,
  keyHint,
  langs,
  lsClose,
  lsOpen,
  lsSuggest,
  transferDefaults,
  transferStart,
  updateCheck,
  vaultRekey,
  type Host,
  type HostForm,
  type Suggest,
  type UpdateStatus,
} from "./api";
import { t } from "./i18n";
import { ICON } from "./ui";

/** Janela modal: Esc fecha (o App ignora as teclas enquanto ela está aberta). */
export function Modal({
  title,
  width,
  scroll,
  onClose,
  children,
  footer,
}: {
  title: ReactNode;
  width?: number;
  /** rola por dentro quando não cabe na tela (formulários longos) */
  scroll?: boolean;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // sem campo com autofocus, o foco vai para a janela (para o Esc funcionar)
    if (!box.current?.contains(document.activeElement)) box.current?.focus();
  }, []);
  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        ref={box}
        className={`modal${scroll ? " scroll" : ""}`}
        tabIndex={-1}
        style={width ? { width } : undefined}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape" && !e.defaultPrevented) {
            e.preventDefault();
            onClose();
          }
        }}
      >
        <h3>{title}</h3>
        {children}
        {footer && <div className="row-btns">{footer}</div>}
      </div>
    </div>
  );
}

/** Pergunta de sim/não. */
export function Confirm({
  title,
  text,
  yes,
  no,
  danger,
  onYes,
  onNo,
  children,
}: {
  title: string;
  text: string;
  yes: string;
  no?: string;
  danger?: boolean;
  onYes: () => void;
  onNo: () => void;
  children?: ReactNode;
}) {
  return (
    <Modal
      title={title}
      width={460}
      onClose={onNo}
      footer={
        <>
          <button className="btn" onClick={onNo}>
            {no ?? t("gui.cancel")}
          </button>
          <button className={`btn ${danger ? "danger-solid" : "primary"}`} autoFocus onClick={onYes}>
            {yes}
          </button>
        </>
      }
    >
      <p className="pre">{text}</p>
      {children}
    </Modal>
  );
}

// --- formulário de host ---

const EMPTY: HostForm = {
  kind: "Ssh",
  alias: "",
  hostname: "",
  user: "",
  port: "",
  identityFile: "",
  password: "",
  notes: "",
  domain: "",
  resolution: "",
  gateway: "",
  sftpPort: "",
  ftpPort: "",
};

const defaultPort = (kind: HostForm["kind"]) => (kind === "Ssh" ? "22" : "3389");

export function HostFormModal({
  editing,
  hosts,
  onSaved,
  onClose,
}: {
  editing: string | null;
  hosts: Host[];
  onSaved: (hosts: Host[], alias: string) => void;
  onClose: () => void;
}) {
  const [form, setForm] = useState<HostForm | null>(editing ? null : EMPTY);
  const [hint, setHint] = useState("");
  const [showPw, setShowPw] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    keyHint().then(setHint, () => {});
    if (editing) hostGet(editing).then(setForm, (e) => setError(String(e)));
  }, [editing]);

  if (!form) return null;
  const ssh = form.kind === "Ssh";
  const set = (k: keyof HostForm) => (v: string) => setForm({ ...form, [k]: v });

  function setKind(kind: HostForm["kind"]) {
    if (!form || kind === form.kind) return;
    // a porta padrão acompanha o tipo
    const port = form.port.trim() === defaultPort(form.kind) ? "" : form.port;
    setForm({ ...form, kind, port });
  }

  async function save() {
    if (!form || busy) return;
    setBusy(true);
    try {
      onSaved(await hostSave(editing, form), form.alias.trim());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const gateways = hosts.filter((h) => h.kind === "ssh" && h.alias !== editing);
  const field = (
    label: string,
    key: keyof HostForm,
    opts: { placeholder?: string; mono?: boolean; autoFocus?: boolean; list?: string } = {},
  ) => (
    <label className="form-row">
      <span className="form-label">{label}</span>
      <input
        className={`input${opts.mono ? " mono" : ""}`}
        value={form[key]}
        placeholder={opts.placeholder}
        autoFocus={opts.autoFocus}
        list={opts.list}
        spellCheck={false}
        onChange={(e) => set(key)(e.target.value)}
      />
    </label>
  );

  return (
    <Modal
      title={editing ? t("form.edit") : t("form.new")}
      width={560}
      scroll
      onClose={onClose}
      footer={
        <>
          <span className="footer-note">
            <kbd>Ctrl+S</kbd> {t("gui.save").toLowerCase()}
          </span>
          <button className="btn" onClick={onClose}>
            {t("gui.cancel")}
          </button>
          <button className="btn primary" disabled={busy} onClick={save}>
            {t("gui.save")}
          </button>
        </>
      }
    >
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
        onKeyDown={(e) => {
          if (e.ctrlKey && e.key.toLowerCase() === "s") {
            e.preventDefault();
            void save();
          }
        }}
      >
        <div className="form-row">
          <span className="form-label">{t("field.kind")}</span>
          <div className="segmented">
            <button type="button" className={ssh ? "on" : ""} onClick={() => setKind("Ssh")}>
              <span className="nf">{ICON.ssh}</span> SSH
            </button>
            <button type="button" className={ssh ? "" : "on"} onClick={() => setKind("Rdp")}>
              <span className="nf">{ICON.rdp}</span> RDP
            </button>
          </div>
        </div>
        {field(t("field.alias"), "alias", { autoFocus: !editing, placeholder: "prod" })}
        {field(t("field.hostname"), "hostname", { mono: true, placeholder: "192.168.0.10" })}
        {field(t("field.user"), "user", {
          placeholder: ssh ? t("gui.optional") : undefined,
        })}
        {!ssh && field(t("field.domain"), "domain", { placeholder: t("ph.domain") })}
        {field(t("field.port"), "port", {
          mono: true,
          placeholder: t("gui.port_default", { port: defaultPort(form.kind) }),
        })}
        {ssh && field(t("field.identity"), "identityFile", { mono: true, placeholder: hint })}
        {ssh &&
          field(t("field.sftp_port"), "sftpPort", {
            mono: true,
            placeholder: t("gui.ph_sftp_port", { port: form.port.trim() || "22" }),
          })}
        {ssh && field(t("field.ftp_port"), "ftpPort", { mono: true, placeholder: t("gui.ph_ftp_port") })}
        {!ssh && field(t("field.resolution"), "resolution", { placeholder: t("gui.ph_resolution") })}
        {!ssh &&
          field(t("field.gateway"), "gateway", { placeholder: t("ph.gateway"), list: "gateways" })}
        <datalist id="gateways">
          {gateways.map((h) => (
            <option key={h.alias} value={h.alias} />
          ))}
        </datalist>
        <label className="form-row">
          <span className="form-label">{t("field.password")}</span>
          <div className="input-with-btn">
            <input
              className="input"
              type={showPw ? "text" : "password"}
              value={form.password}
              placeholder={t("gui.no_pw_ph")}
              onChange={(e) => set("password")(e.target.value)}
            />
            <button type="button" className="btn ghost" onClick={() => setShowPw((s) => !s)}>
              {showPw ? t("gui.hide") : t("gui.show")}
            </button>
          </div>
        </label>
        <label className="form-row top">
          <span className="form-label">{t("field.notes")}</span>
          <textarea
            className="input"
            rows={3}
            value={form.notes}
            onChange={(e) => set("notes")(e.target.value)}
          />
        </label>
        {error && <div className="form-error">{error}</div>}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}

// --- rsync ---

const ROWS = 8;

export function TransferModal({
  host,
  onStarted,
  onClose,
}: {
  host: Host;
  onStarted: (msg: string) => void;
  onClose: () => void;
}) {
  const [upload, setUpload] = useState(false);
  const [remote, setRemote] = useState("");
  const [local, setLocal] = useState("");
  const [focus, setFocus] = useState<"remote" | "local">("remote");
  const [lister, setLister] = useState<number | null>(null);
  const [listerError, setListerError] = useState<string | null>(null);
  const [sugg, setSugg] = useState<Suggest | null>(null);
  const [hi, setHi] = useState<number | null>(null);
  /** Esc esconde as sugestões até o campo mudar */
  const [hidden, setHidden] = useState(false);
  /** algum campo de caminho está com o foco; sem foco, a lista some e libera o resto do popup */
  const [editing, setEditing] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [poll, setPoll] = useState(0);
  const inputs = useRef<Record<string, HTMLInputElement | null>>({});
  /** depois de completar, o cursor vai para o fim (e o campo rola até ele) */
  const toEnd = useRef(false);

  // conexão escondida para listar as pastas do servidor enquanto o popup está aberto
  useEffect(() => {
    let id: number | null = null;
    let closed = false;
    transferDefaults().then(setLocal, () => {});
    lsOpen(host.alias).then(
      (n) => {
        if (closed) void lsClose(n);
        else setLister((id = n));
      },
      (e) => setListerError(String(e)),
    );
    return () => {
      closed = true;
      if (id !== null) void lsClose(id);
    };
  }, [host.alias]);

  const value = focus === "remote" ? remote : local;
  const setValue = focus === "remote" ? setRemote : setLocal;

  useEffect(() => {
    let stop = false;
    if (focus === "remote" && lister === null) {
      setSugg(
        listerError
          ? { state: "failed", error: listerError, items: [] }
          : { state: "loading", error: "", items: [] },
      );
      return;
    }
    lsSuggest(focus === "remote" ? lister : null, value).then((s) => {
      if (stop) return;
      setSugg(s);
      // a listagem do servidor chega aos poucos: pergunta de novo em seguida
      if (s.state === "loading") setTimeout(() => setPoll((p) => p + 1), 150);
    });
    return () => {
      stop = true;
    };
  }, [focus, value, lister, listerError, poll]);

  useEffect(() => {
    const el = inputs.current[focus];
    if (!toEnd.current || !el) return;
    toEnd.current = false;
    el.setSelectionRange(el.value.length, el.value.length);
    el.scrollLeft = el.scrollWidth;
  }, [value, focus]);

  useEffect(() => setHidden(false), [value, focus]);

  const items = sugg?.state === "ready" && !hidden ? sugg.items : [];
  const dropdownOpen =
    editing && !!sugg && !hidden && (sugg.state !== "ready" || items.length > 0);
  const dir = value.slice(0, value.lastIndexOf("/") + 1);

  function accept(i: number) {
    toEnd.current = true;
    setValue(dir + items[i]);
    setHi(null);
  }

  function completeCommon() {
    if (!items.length) return;
    let common = items[0];
    for (const it of items) while (!it.startsWith(common)) common = common.slice(0, -1);
    const next = dir + common;
    if (next.length > value.length) {
      toEnd.current = true;
      setValue(next);
    }
    else setHi(0);
  }

  function onKey(e: React.KeyboardEvent) {
    if (e.key === "Tab" && !e.ctrlKey) {
      if (hi !== null && items.length) {
        setHi((hi + (e.shiftKey ? items.length - 1 : 1)) % items.length);
      } else if (!e.shiftKey) completeCommon();
      else return;
    } else if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !dropdownOpen) {
      // sem sugestões, as setas trocam de campo
      inputs.current[focus === "remote" ? "local" : "remote"]?.focus();
    } else if (e.key === "ArrowDown" && items.length) {
      setHi(hi === null ? 0 : (hi + 1) % items.length);
    } else if (e.key === "ArrowUp" && items.length) {
      setHi(hi === null ? items.length - 1 : (hi + items.length - 1) % items.length);
    } else if (e.key === "Enter") {
      if (hi !== null && items[hi] !== undefined) accept(hi);
      else void start();
    } else if (e.key === "Escape" && dropdownOpen) {
      setHi(null);
      setHidden(true);
    } else return;
    e.preventDefault();
  }

  async function start() {
    try {
      onStarted(await transferStart(host.alias, upload, remote, local));
    } catch (e) {
      setError(String(e));
    }
  }

  const from = upload ? local : `${host.alias}:${remote}`;
  const to = upload ? `${host.alias}:${remote}` : local;

  const dropdown = (which: "remote" | "local") => {
    if (focus !== which || !sugg || !dropdownOpen) return null;
    return (
      // a lista do Servidor abre para cima, para não tampar o campo Local
      <div className={`suggest${which === "remote" ? " up" : ""}`}>
        {sugg.state === "loading" && <div className="suggest-msg">{t("transfer.loading")}</div>}
        {sugg.state === "failed" && (
          <div className="suggest-msg">{t("transfer.no_suggestions", { err: sugg.error })}</div>
        )}
        {items.slice(0, 200).map((it, i) => (
          <div
            key={it}
            ref={(el) => {
              if (i === hi) el?.scrollIntoView({ block: "nearest" });
            }}
            className={`suggest-item${it.endsWith("/") ? " dir" : ""}${i === hi ? " hi" : ""}`}
            onMouseDown={(e) => {
              e.preventDefault();
              accept(i);
            }}
          >
            {printable(it)}
          </div>
        ))}
      </div>
    );
  };

  const pathInput = (which: "remote" | "local") => (
    <div className="suggest-wrap">
      <input
        ref={(el) => {
          inputs.current[which] = el;
        }}
        className="input mono"
        autoFocus={which === "remote"}
        value={which === "remote" ? remote : local}
        placeholder={which === "remote" ? t("transfer.remote_ph") : ""}
        spellCheck={false}
        onFocus={() => {
          setFocus(which);
          setEditing(true);
          setHi(null);
        }}
        onBlur={() => {
          setEditing(false);
          setHi(null);
        }}
        onChange={(e) => {
          (which === "remote" ? setRemote : setLocal)(e.target.value);
          setHi(null);
        }}
        onKeyDown={onKey}
      />
      {dropdown(which)}
    </div>
  );

  return (
    <Modal
      title={
        <>
          {t("transfer.title")} <span className="muted">· {host.alias}</span>
        </>
      }
      width={640}
      onClose={onClose}
      footer={
        <>
          <span className="footer-note">
            <kbd>Tab</kbd> {t("h.complete")}
          </span>
          <button className="btn" onClick={onClose}>
            {t("gui.cancel")}
          </button>
          <button className="btn primary" onClick={() => void start()}>
            <span className="nf">{ICON.transfer}</span> {t("gui.start")}
          </button>
        </>
      }
    >
      <div className="form">
        <div className="form-row">
          <span className="form-label">{t("transfer.direction")}</span>
          <div className="segmented">
            <button type="button" className={upload ? "" : "on"} onClick={() => setUpload(false)}>
              <span className="nf">{ICON.download}</span> {t("transfer.down")}
            </button>
            <button type="button" className={upload ? "on" : ""} onClick={() => setUpload(true)}>
              <span className="nf">{ICON.upload}</span> {t("transfer.up")}
            </button>
          </div>
        </div>
        <div className="form-row">
          <span className="form-label">{t("transfer.remote")}</span>
          {pathInput("remote")}
        </div>
        <div className="form-row">
          <span className="form-label">{t("transfer.local")}</span>
          {pathInput("local")}
        </div>
        <div className="transfer-preview mono">
          <span>{from || "…"}</span>
          <span className="muted"> → </span>
          <span>{to || "…"}</span>
        </div>
        <div className="tips">
          <div>{t("transfer.tip_slash")}</div>
          <div>{t("transfer.tip_home")}</div>
        </div>
        {error && <div className="form-error">{error}</div>}
      </div>
    </Modal>
  );
}

/** Nomes com caracteres de controle (ex.: um arquivo criado sem querer colando um prompt com
 *  cores) aparecem com eles visíveis, em vez de sumirem ou bagunçarem a linha. */
function printable(name: string) {
  return name.replace(/[\u0000-\u001f\u007f]/g, (c) =>
    c === "\u001b" ? "^[" : `\\x${c.charCodeAt(0).toString(16).padStart(2, "0")}`,
  );
}

// --- idioma, senha mestra, atalhos ---

export function LangModal({ onPick, onClose }: { onPick: (code: string) => void; onClose: () => void }) {
  const [list, setList] = useState<{ code: string; name: string }[]>([]);
  const [current, setCurrent] = useState("");
  useEffect(() => {
    langs().then(([l, c]) => {
      setList(l);
      setCurrent(c);
    });
  }, []);
  return (
    <Modal title={t("gui.language")} width={320} onClose={onClose}>
      <div className="choice-list">
        {list.map((l) => (
          <button
            key={l.code}
            className={`choice${l.code === current ? " on" : ""}`}
            autoFocus={l.code === current}
            onClick={() => onPick(l.code)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                const sib =
                  e.key === "ArrowDown"
                    ? e.currentTarget.nextElementSibling
                    : e.currentTarget.previousElementSibling;
                (sib as HTMLElement | null)?.focus();
              }
            }}
          >
            {l.name}
            {l.code === current && <span className="nf accent-text">{ICON.check}</span>}
          </button>
        ))}
      </div>
    </Modal>
  );
}

export function RekeyModal({ onDone, onClose }: { onDone: (msg: string) => void; onClose: () => void }) {
  const [a, setA] = useState("");
  const [b, setB] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function save() {
    if (a.length < 4) return setError(t("pw.min_len"));
    if (a !== b) return setError(t("pw.mismatch"));
    try {
      onDone(await vaultRekey(a));
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <Modal
      title={t("pw.title_rekey")}
      width={420}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {t("gui.cancel")}
          </button>
          <button className="btn primary" onClick={() => void save()}>
            {t("gui.save")}
          </button>
        </>
      }
    >
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <input
          className="input"
          type="password"
          autoFocus
          placeholder={t("gui.new_master")}
          value={a}
          onChange={(e) => setA(e.target.value)}
        />
        <input
          className="input"
          type="password"
          placeholder={t("gui.confirm_master")}
          value={b}
          onChange={(e) => setB(e.target.value)}
        />
        <p className="muted small">{t("pw.warning")}</p>
        {error && <div className="form-error">{error}</div>}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}

export function HelpModal({ update, onClose }: { update: UpdateStatus | null; onClose: () => void }) {
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState("");
  function check() {
    setChecking(true);
    setResult("");
    updateCheck()
      .then((s) => {
        if (s.state === "available") setResult(t("gui.upd_found", { v: s.version ?? "" }));
        else if (s.state === "none") setResult(t("gui.upd_none", { v: s.current }));
        else if (s.state === "error") setResult(t("gui.upd_error", { e: s.error ?? "" }));
        else if (s.state === "ready") setResult(t("gui.upd_ready", { v: s.version ?? "" }));
      }, (e) => setResult(t("gui.upd_error", { e: String(e) })))
      .finally(() => setChecking(false));
  }
  const list: [string, string][] = [
    ["Enter", t("gui.help_enter")],
    ["x", t("gui.help_x")],
    ["K", t("help.k")],
    ["r", t("help.r")],
    ["f", t("files.help_f")],
    ["c", t("help.c")],
    ["a", t("help.a")],
    ["e", t("help.e")],
    ["d", t("help.d")],
    ["/", t("help.filter")],
    ["Shift+U", t("help.u")],
    ["i", t("help.i")],
    ["P", t("help.rekey")],
    ["L", t("help.lang")],
    ["Tab", t("help.tab")],
    ["↑/↓ j/k", t("help.nav")],
    ["q", t("help.quit")],
  ];
  const term: [string, string][] = [
    ["Ctrl+]", t("help.panel")],
    ["Alt+P", t("help.altp")],
    ["Alt+←/→", t("help.switch")],
    ["Shift+PgUp/PgDn", t("help.scroll")],
    ["Alt+I", t("gui.panel")],
  ];
  const table = (rows: [string, string][]) => (
    <div className="help-grid">
      {rows.map(([k, d]) => (
        <div key={k} style={{ display: "contents" }}>
          <span>
            <kbd>{k}</kbd>
          </span>
          <span>{d}</span>
        </div>
      ))}
    </div>
  );
  return (
    <Modal
      title={t("gui.shortcuts")}
      width={640}
      onClose={onClose}
      footer={
        <>
          <span className="footer-note">
            sshdeck {update ? t("gui.upd_version", { v: update.current }) : ""}
            {result && <> · {result}</>}
          </span>
          <button className="btn" disabled={checking} onClick={check}>
            <span className={`nf${checking ? " spin" : ""}`}>{checking ? ICON.spinner : ICON.refresh}</span>{" "}
            {checking ? t("gui.upd_checking") : t("gui.upd_check")}
          </button>
        </>
      }
    >
      <div className="help-cols">
        <div>
          <h4>{t("gui.in_list")}</h4>
          {table(list)}
        </div>
        <div>
          <h4>{t("gui.in_terminal")}</h4>
          {table(term)}
          <h4 style={{ marginTop: 18 }}>{t("help.tip_key")}</h4>
          <p className="muted">{t("help.tip")}</p>
        </div>
      </div>
    </Modal>
  );
}
