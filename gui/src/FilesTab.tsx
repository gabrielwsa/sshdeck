import { useCallback, useEffect, useRef, useState } from "react";
import { Channel } from "@tauri-apps/api/core";
import {
  type CopyEvent,
  type FileEntry,
  type FileProto,
  filesCancel,
  filesClose,
  filesCopy,
  filesExisting,
  filesList,
  filesMkdir,
  filesOpen,
  filesRemove,
  filesRename,
} from "./api";
import { t } from "./i18n";
import { Confirm, Modal } from "./modals";
import { ICON } from "./ui";

/** Uma coluna: o computador (remote = false) ou o servidor. */
type Pane = {
  path: string;
  entries: FileEntry[];
  /** caminhos selecionados */
  sel: string[];
  /** linha com o cursor do teclado */
  cursor: number;
  loading: boolean;
  error: string;
  /** itens da pasta (com o filtro); `entries` pode trazer menos, por causa do limite */
  total: number;
  /** filtro por nome (procura na pasta inteira, não só no que foi carregado) */
  filter: string;
};

type Job = {
  id: number;
  upload: boolean;
  label: string;
  done: number;
  total: number;
  file: string;
  filesDone: number;
  filesTotal: number;
  rate: number;
  finished: boolean;
  error: string | null;
  cancelled: boolean;
  /** pasta de destino, recarregada no fim */
  dest: string;
};

type Dialog =
  | { kind: "mkdir"; side: number }
  | { kind: "rename"; side: number; entry: FileEntry }
  | { kind: "delete"; side: number; items: FileEntry[] }
  | { kind: "overwrite"; from: number; items: FileEntry[]; names: string[] };

const emptyPane = (): Pane => ({
  path: "",
  entries: [],
  sel: [],
  cursor: 0,
  loading: true,
  error: "",
  total: 0,
  filter: "",
});

/** Quantos itens carregar por vez (0 = todos); a escolha fica salva. */
const LIMITS = [200, 500, 1000, 0];
const LIMIT_KEY = "sshdeck.files.limit";
const savedLimit = () => {
  const n = Number(localStorage.getItem(LIMIT_KEY) ?? 500);
  return LIMITS.includes(n) ? n : 500;
};

export function fmtSize(n: number) {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

const pad = (n: number) => String(n).padStart(2, "0");
function fmtDate(secs: number | null) {
  if (!secs) return "";
  const d = new Date(secs * 1000);
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Pasta de cima de um caminho POSIX. */
function parent(path: string) {
  const p = path.replace(/\/+$/, "");
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}
const join = (dir: string, name: string) => (dir.endsWith("/") ? dir + name : `${dir}/${name}`);

type Props = {
  alias: string;
  proto: FileProto;
  visible: boolean;
  focused: boolean;
  onStatus: (msg: string) => void;
  /** a conexão não abriu (a aba fecha e o motivo vai para a barra de status) */
  onFail: (err: string) => void;
  onReady: () => void;
  onClick: () => void;
};

/**
 * Navegador de arquivos em duas colunas: à esquerda o computador, à direita o servidor (SFTP ou
 * FTP). Copia arrastando entre as colunas, com duplo clique num arquivo ou com F5.
 */
export default function FilesTab({ alias, proto, visible, focused, onStatus, onFail, onReady, onClick }: Props) {
  const [rid, setRid] = useState<number | null>(null);
  const [panes, setPanes] = useState<[Pane, Pane]>([emptyPane(), emptyPane()]);
  const [side, setSide] = useState(1);
  const [hidden, setHidden] = useState(false);
  const [limit, setLimit] = useState(savedLimit);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [pathEdit, setPathEdit] = useState<[string | null, string | null]>([null, null]);
  const root = useRef<HTMLDivElement>(null);
  const lists = [useRef<HTMLDivElement>(null), useRef<HTMLDivElement>(null)];
  const anchor = useRef<[number, number]>([0, 0]);
  const ridRef = useRef<number | null>(null);
  // o load lê sempre os valores atuais (é chamado de callbacks antigos, como o fim de uma cópia)
  const opts = useRef({ limit, hidden, panes });
  opts.current = { limit, hidden, panes };
  const filterTimer = useRef(0);
  /** filtro atual de cada coluna (a recarga lê daqui, sem esperar o estado) */
  const filters = useRef<[string, string]>(["", ""]);
  const filterInputs = [useRef<HTMLInputElement>(null), useRef<HTMLInputElement>(null)];
  const cb = useRef({ onStatus, onFail, onReady });
  cb.current = { onStatus, onFail, onReady };

  const idOf = (s: number) => (s === 0 ? null : ridRef.current);
  // ocultos, filtro e limite já vêm aplicados do Rust
  const shown = (p: Pane) => p.entries;

  const setPane = (s: number, f: (p: Pane) => Pane) =>
    setPanes((ps) => {
      const next: [Pane, Pane] = [ps[0], ps[1]];
      next[s] = f(ps[s]);
      return next;
    });

  /**
   * Lista a pasta. `keepSel`: recarga da mesma pasta (mantém seleção e quantos já tinham sido
   * carregados); `more`: "Carregar mais", pede mais um bloco do limite.
   */
  const load = useCallback(async (s: number, path: string, keepSel = false, more = false) => {
    const { limit, hidden, panes } = opts.current;
    const cur = panes[s];
    const samePath = cur.path === path;
    // pasta nova: o filtro da anterior não vale mais
    if (!samePath) filters.current[s] = "";
    const filter = filters.current[s];
    const want =
      limit === 0 ? 0 : more ? cur.entries.length + limit : keepSel && samePath ? Math.max(limit, cur.entries.length) : limit;
    setPane(s, (p) => ({ ...p, loading: true, error: "" }));
    try {
      const r = await filesList(s === 0 ? null : ridRef.current, path, { limit: want, filter, hidden });
      setPane(s, (p) => {
        const same = p.path === r.path;
        return {
          path: r.path,
          entries: r.entries,
          total: r.total,
          filter: same ? p.filter : "",
          sel: keepSel && same ? p.sel.filter((x) => r.entries.some((e) => e.path === x)) : [],
          cursor: same ? Math.min(p.cursor, Math.max(r.entries.length - 1, 0)) : 0,
          loading: false,
          error: "",
        };
      });
    } catch (e) {
      setPane(s, (p) => ({ ...p, loading: false, error: String(e) }));
    }
  }, []);

  // conecta ao abrir a aba e fecha a conexão ao sair
  useEffect(() => {
    let closed = false;
    void load(0, "~");
    filesOpen(alias, proto).then(
      (o) => {
        if (closed) return void filesClose(o.id);
        ridRef.current = o.id;
        setRid(o.id);
        cb.current.onReady();
        void load(1, o.home);
      },
      (e) => !closed && cb.current.onFail(String(e)),
    );
    return () => {
      closed = true;
      if (ridRef.current !== null) void filesClose(ridRef.current);
    };
  }, [alias, proto, load]);

  useEffect(() => {
    if (visible && focused && !dialog) lists[side].current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, focused, side, dialog]);

  // o cursor sempre visível na lista
  useEffect(() => {
    const list = lists[side].current;
    const row = list?.querySelector(".frow.cursor");
    row?.scrollIntoView({ block: "nearest" });
    // no último item, mostra também o "Carregar mais" que vem embaixo
    const p = panes[side];
    if (list && p.entries.length && p.cursor === p.entries.length - 1 && p.entries.length < p.total)
      list.scrollTop = list.scrollHeight;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [panes, side]);

  /** O que uma ação usa: a seleção, ou a linha do cursor se nada estiver selecionado. */
  function targets(s: number): FileEntry[] {
    const p = panes[s];
    const list = shown(p);
    const sel = list.filter((e) => p.sel.includes(e.path));
    if (sel.length) return sel;
    return list[p.cursor] ? [list[p.cursor]] : [];
  }

  function open(s: number, e: FileEntry) {
    if (e.dir) void load(s, e.path);
    else copy(s, [e]);
  }

  /** Copia para a pasta aberta na outra coluna; pergunta antes de sobrescrever. */
  async function copy(from: number, items: FileEntry[], force = false) {
    if (!items.length || ridRef.current === null) return;
    const to = 1 - from;
    if (!force) {
      // pergunta ao destino (a lista na tela pode estar cortada pelo limite)
      let names: string[] = [];
      try {
        names = await filesExisting(idOf(to), panes[to].path, items.map((i) => i.name));
      } catch {
        names = items.map((i) => i.name).filter((n) => panes[to].entries.some((e) => e.name === n));
      }
      if (names.length) return setDialog({ kind: "overwrite", from, items, names });
    }
    const upload = from === 0;
    const dest = panes[to].path;
    const label =
      items.length === 1 ? items[0].name : t("files.n_items", { n: items.length });
    const channel = new Channel<CopyEvent>();
    let jobId = 0;
    channel.onmessage = (ev) => {
      setJobs((js) =>
        js.map((j) => {
          if (j.id !== jobId) return j;
          if (ev.type === "progress") return { ...j, ...ev };
          return { ...j, finished: true, error: ev.error, cancelled: ev.cancelled };
        }),
      );
      if (ev.type === "finished") {
        void load(to, panes[to].path, true);
        if (ev.error) cb.current.onStatus(t("files.copy_failed", { name: label, err: ev.error }));
        else if (ev.cancelled) cb.current.onStatus(t("files.copy_cancelled", { name: label }));
        else {
          cb.current.onStatus(t(upload ? "files.uploaded" : "files.downloaded", { name: label }));
          // sucesso some sozinho; erro fica até fechar
          window.setTimeout(() => setJobs((js) => js.filter((j) => j.id !== jobId)), 4000);
        }
      }
    };
    filesCopy(ridRef.current, upload, items.map((i) => ({ path: i.path, dir: i.dir })), dest, channel).then(
      (id) => {
        jobId = id;
        setJobs((js) => [
          ...js,
          {
            id,
            upload,
            label,
            done: 0,
            total: 0,
            file: "",
            filesDone: 0,
            filesTotal: 0,
            rate: 0,
            finished: false,
            error: null,
            cancelled: false,
            dest,
          },
        ]);
      },
      (e) => cb.current.onStatus(t("files.copy_failed", { name: label, err: String(e) })),
    );
  }

  function select(s: number, idx: number, e: { ctrlKey: boolean; shiftKey: boolean }) {
    setSide(s);
    const list = shown(panes[s]);
    const item = list[idx];
    if (!item) return;
    setPane(s, (p) => {
      let sel: string[];
      if (e.shiftKey) {
        const [a, b] = [Math.min(anchor.current[s], idx), Math.max(anchor.current[s], idx)];
        sel = list.slice(a, b + 1).map((x) => x.path);
      } else if (e.ctrlKey) {
        sel = p.sel.includes(item.path) ? p.sel.filter((x) => x !== item.path) : [...p.sel, item.path];
        anchor.current[s] = idx;
      } else {
        sel = [item.path];
        anchor.current[s] = idx;
      }
      return { ...p, sel, cursor: idx };
    });
  }

  function onKey(s: number, e: React.KeyboardEvent) {
    const p = panes[s];
    const list = shown(p);
    const k = e.key;
    const moveTo = (idx: number) => {
      const i = Math.max(0, Math.min(list.length - 1, idx));
      if (e.shiftKey) select(s, i, { ctrlKey: false, shiftKey: true });
      else {
        anchor.current[s] = i;
        setPane(s, (x) => ({ ...x, cursor: i, sel: [] }));
      }
    };
    let handled = true;
    if (k === "ArrowDown" && p.cursor === list.length - 1 && list.length < p.total && !p.loading)
      void load(s, p.path, true, true).then(() =>
        setPane(s, (x) => ({ ...x, cursor: Math.min(x.cursor + 1, x.entries.length - 1) })),
      );
    else if (k === "ArrowDown") moveTo(p.cursor + 1);
    else if (k === "ArrowUp") moveTo(p.cursor - 1);
    else if (k === "PageDown") moveTo(p.cursor + 15);
    else if (k === "PageUp") moveTo(p.cursor - 15);
    else if (k === "Home") moveTo(0);
    else if (k === "End") moveTo(list.length - 1);
    else if (k === "Tab") setSide(1 - s);
    else if (k === "Enter" && list[p.cursor]) open(s, list[p.cursor]);
    else if (k === "Backspace") void load(s, parent(p.path));
    else if (k === " " || k === "Insert") {
      const item = list[p.cursor];
      if (item)
        setPane(s, (x) => ({
          ...x,
          sel: x.sel.includes(item.path) ? x.sel.filter((y) => y !== item.path) : [...x.sel, item.path],
          cursor: Math.min(x.cursor + 1, list.length - 1),
        }));
    } else if (e.ctrlKey && (k === "a" || k === "A"))
      setPane(s, (x) => ({ ...x, sel: list.map((y) => y.path) }));
    else if (k === "F5") copy(s, targets(s));
    else if (k === "F7") setDialog({ kind: "mkdir", side: s });
    else if (k === "F2" && list[p.cursor]) setDialog({ kind: "rename", side: s, entry: list[p.cursor] });
    else if ((k === "Delete" || k === "F8") && targets(s).length)
      setDialog({ kind: "delete", side: s, items: targets(s) });
    else if (e.ctrlKey && (k === "h" || k === "H")) setHidden((h) => !h);
    else if (e.ctrlKey && (k === "f" || k === "F")) filterInputs[s].current?.focus();
    else if (k === "Escape" && p.filter) setFilter(s, "", true);
    else if (e.ctrlKey && (k === "r" || k === "R")) void load(s, p.path, true);
    else if (e.ctrlKey && (k === "l" || k === "L")) setPathEdit((pe) => (s === 0 ? [p.path, pe[1]] : [pe[0], p.path]));
    else handled = false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  /** Filtro por nome: espera a digitação parar para listar de novo. */
  function setFilter(s: number, value: string, now = false) {
    setPane(s, (p) => ({ ...p, filter: value }));
    filters.current[s] = value;
    window.clearTimeout(filterTimer.current);
    const run = () => void load(s, opts.current.panes[s].path, true);
    if (now) run();
    else filterTimer.current = window.setTimeout(run, 250);
  }

  // ocultos e limite valem para as duas colunas: lista de novo
  const firstOpts = useRef(true);
  useEffect(() => {
    if (firstOpts.current) {
      firstOpts.current = false;
      return;
    }
    localStorage.setItem(LIMIT_KEY, String(limit));
    if (opts.current.panes[0].path) void load(0, opts.current.panes[0].path);
    if (ridRef.current !== null && opts.current.panes[1].path) void load(1, opts.current.panes[1].path);
  }, [hidden, limit, load]);

  async function act(f: () => Promise<unknown>, s: number) {
    try {
      await f();
    } catch (e) {
      cb.current.onStatus(t("files.op_failed", { err: String(e) }));
    }
    setDialog(null);
    void load(s, panes[s].path, true);
  }

  const paneView = (s: number) => {
    const p = panes[s];
    const list = shown(p);
    const remote = s === 1;
    const editing = pathEdit[s];
    const title = remote ? `${proto.toUpperCase()} · ${alias}` : t("files.local");
    return (
      <div
        className={`fpane${side === s ? " active" : ""}`}
        onMouseDown={() => setSide(s)}
        onDragOver={(e) => {
          if (e.dataTransfer.types.includes(`sshdeck/from-${1 - s}`)) e.preventDefault();
        }}
        onDrop={(e) => {
          const raw = e.dataTransfer.getData(`sshdeck/from-${1 - s}`);
          if (!raw) return;
          e.preventDefault();
          const paths: string[] = JSON.parse(raw);
          copy(1 - s, panes[1 - s].entries.filter((x) => paths.includes(x.path)));
        }}
      >
        <div className="fhead">
          <span className="ftitle">
            <span className="nf">{remote ? (proto === "ftp" ? ICON.globe : ICON.server) : ICON.laptop}</span>{" "}
            {title}
          </span>
          <div className="fpath">
            <button className="icon-btn" title={`${t("files.up")} (Backspace)`} onClick={() => void load(s, parent(p.path))}>
              {ICON.up}
            </button>
            {editing === null ? (
              <span
                className="path mono"
                title={`${p.path} (Ctrl+L)`}
                onDoubleClick={() => setPathEdit((pe) => (s === 0 ? [p.path, pe[1]] : [pe[0], p.path]))}
              >
                <bdi>{p.path}</bdi>
              </span>
            ) : (
              <input
                className="input mono"
                autoFocus
                value={editing}
                spellCheck={false}
                onChange={(e) => {
                  const v = e.target.value;
                  setPathEdit((pe) => (s === 0 ? [v, pe[1]] : [pe[0], v]));
                }}
                onBlur={() => setPathEdit((pe) => (s === 0 ? [null, pe[1]] : [pe[0], null]))}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") {
                    void load(s, editing);
                    setPathEdit((pe) => (s === 0 ? [null, pe[1]] : [pe[0], null]));
                    lists[s].current?.focus();
                  } else if (e.key === "Escape") {
                    setPathEdit((pe) => (s === 0 ? [null, pe[1]] : [pe[0], null]));
                    lists[s].current?.focus();
                  }
                }}
              />
            )}
            <button className="icon-btn" title={`${t("files.refresh")} (Ctrl+R)`} onClick={() => void load(s, p.path, true)}>
              {ICON.refresh}
            </button>
            <button className="icon-btn" title={`${t("files.mkdir")} (F7)`} onClick={() => setDialog({ kind: "mkdir", side: s })}>
              {ICON.folderPlus}
            </button>
          </div>
          <label className="ffilter">
            <span className="nf">{ICON.search}</span>
            <input
              ref={filterInputs[s]}
              value={p.filter}
              placeholder={t("files.filter")}
              spellCheck={false}
              onChange={(e) => setFilter(s, e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Escape") {
                  if (p.filter) setFilter(s, "", true);
                  lists[s].current?.focus();
                } else if (e.key === "Enter" || e.key === "ArrowDown") {
                  e.preventDefault();
                  lists[s].current?.focus();
                }
              }}
            />
            {!p.filter && <kbd>Ctrl+F</kbd>}
          </label>
        </div>
        <div className="fcols">
          <span>{t("files.name")}</span>
          <span className="num">{t("files.size")}</span>
          <span>{t("files.modified")}</span>
        </div>
        <div
          ref={lists[s]}
          className="flist"
          tabIndex={0}
          onKeyDown={(e) => onKey(s, e)}
          onFocus={() => setSide(s)}
        >
          {p.error ? (
            <div className="fmsg red">{p.error}</div>
          ) : remote && rid === null ? (
            <div className="fmsg">
              <span className="nf spin">{ICON.spinner}</span> {t("info.connecting")}
            </div>
          ) : !list.length && !p.loading ? (
            <div className="fmsg faint">{t("files.empty")}</div>
          ) : (
            list.map((e, i) => (
              <div
                key={e.path}
                className={`frow${p.sel.includes(e.path) ? " sel" : ""}${i === p.cursor && side === s ? " cursor" : ""}`}
                draggable
                onDragStart={(ev) => {
                  const items = p.sel.includes(e.path) ? p.sel : [e.path];
                  ev.dataTransfer.setData(`sshdeck/from-${s}`, JSON.stringify(items));
                  ev.dataTransfer.effectAllowed = "copy";
                }}
                onMouseDown={(ev) => {
                  if (ev.button !== 0) return;
                  // arrastar uma seleção não a desfaz
                  if (!ev.ctrlKey && !ev.shiftKey && p.sel.includes(e.path)) {
                    setPane(s, (x) => ({ ...x, cursor: i }));
                    return;
                  }
                  select(s, i, ev);
                }}
                onDoubleClick={() => open(s, e)}
              >
                <span className={`nf ficon${e.dir ? " dir" : ""}`}>
                  {e.dir ? ICON.folder : e.link ? ICON.link : ICON.file}
                </span>
                <span className="fname" title={e.name}>
                  {e.name}
                  {e.link && <span className="nf faint flink">{ICON.link}</span>}
                </span>
                <span className="num faint">{e.dir ? "" : fmtSize(e.size)}</span>
                <span className="faint">{fmtDate(e.mtime)}</span>
              </div>
            ))
          )}
          {!p.error && p.entries.length < p.total && (
            <button className="fmore" disabled={p.loading} onClick={() => void load(s, p.path, true, true)}>
              {t("files.more", { n: Math.min(limit, p.total - p.entries.length), left: p.total - p.entries.length })}
            </button>
          )}
        </div>
        <div className="ffoot faint">
          {list.length < p.total
            ? t("files.count_of", { n: list.length, total: p.total })
            : t("files.count", { n: p.total })}
          {p.sel.length > 0 && ` · ${t("files.selected", { n: p.sel.length })}`}
          {p.loading && <span className="nf spin">{ICON.spinner}</span>}
          <span className="spacer" />
          <label className="flimit" title={t("files.limit_hint")}>
            {t("files.limit")}
            <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
              {LIMITS.map((n) => (
                <option key={n} value={n}>
                  {n === 0 ? t("files.all") : n}
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>
    );
  };

  const d = dialog;
  return (
    <div
      ref={root}
      className="files-tab"
      style={{ display: visible ? "flex" : "none" }}
      onMouseDown={onClick}
    >
      <div className="fpanes">
        {paneView(0)}
        <div className="fmiddle">
          <button
            className="icon-btn"
            title={`${t("files.upload")} (F5)`}
            disabled={rid === null}
            onClick={() => copy(0, targets(0))}
          >
            {ICON.right}
          </button>
          <button
            className="icon-btn"
            title={`${t("files.download")} (F5)`}
            disabled={rid === null}
            onClick={() => copy(1, targets(1))}
          >
            {ICON.left}
          </button>
        </div>
        {paneView(1)}
      </div>

      {jobs.length > 0 && (
        <div className="fjobs">
          {jobs.map((j) => {
            const pct = j.total ? Math.min(100, Math.floor((j.done / j.total) * 100)) : j.finished ? 100 : 0;
            return (
              <div key={j.id} className={`fjob${j.error ? " bad" : ""}`}>
                <span className="nf">{j.upload ? ICON.upload : ICON.download}</span>
                <span className="fname">{j.label}</span>
                <span className="faint nums">
                  {j.error
                    ? j.error
                    : j.cancelled
                      ? t("files.cancelled")
                      : j.finished
                        ? t("files.done")
                        : j.total
                          ? `${pct}% · ${fmtSize(j.done)} / ${fmtSize(j.total)} · ${fmtSize(j.rate)}/s · ${j.filesDone}/${j.filesTotal}`
                          : t("ui.transfer_preparing")}
                </span>
                <div className="bar">
                  <div style={{ width: `${pct}%` }} />
                </div>
                <button
                  className="icon-btn"
                  title={j.finished ? t("gui.close") : t("files.cancel")}
                  onClick={() =>
                    j.finished ? setJobs((js) => js.filter((x) => x.id !== j.id)) : void filesCancel(j.id)
                  }
                >
                  {ICON.close}
                </button>
              </div>
            );
          })}
        </div>
      )}

      {d?.kind === "mkdir" && (
        <Prompt
          title={t("files.mkdir")}
          label={t("files.name")}
          initial=""
          onClose={() => setDialog(null)}
          onOk={(name) => act(() => filesMkdir(idOf(d.side), join(panes[d.side].path, name)), d.side)}
        />
      )}
      {d?.kind === "rename" && (
        <Prompt
          title={t("files.rename")}
          label={t("files.name")}
          initial={d.entry.name}
          onClose={() => setDialog(null)}
          onOk={(name) =>
            act(() => filesRename(idOf(d.side), d.entry.path, join(parent(d.entry.path), name)), d.side)
          }
        />
      )}
      {d?.kind === "delete" && (
        <Confirm
          title={t("files.delete")}
          text={t("files.delete_q", {
            what: d.items.length === 1 ? d.items[0].name : t("files.n_items", { n: d.items.length }),
            where: d.side === 0 ? t("files.local").toLowerCase() : alias,
          })}
          yes={t("gui.delete")}
          danger
          onNo={() => setDialog(null)}
          onYes={() =>
            act(() => filesRemove(idOf(d.side), d.items.map((i) => ({ path: i.path, dir: i.dir && !i.link }))), d.side)
          }
        />
      )}
      {d?.kind === "overwrite" && (
        <Confirm
          title={t("files.overwrite")}
          text={t("files.overwrite_q", { list: d.names.slice(0, 5).join(", ") + (d.names.length > 5 ? "…" : "") })}
          yes={t("files.replace")}
          onNo={() => setDialog(null)}
          onYes={() => {
            setDialog(null);
            copy(d.from, d.items, true);
          }}
        />
      )}
    </div>
  );
}

function Prompt({
  title,
  label,
  initial,
  onOk,
  onClose,
}: {
  title: string;
  label: string;
  initial: string;
  onOk: (v: string) => void;
  onClose: () => void;
}) {
  const [v, setV] = useState(initial);
  const valid = v.trim() !== "" && !v.includes("/");
  return (
    <Modal
      title={title}
      width={420}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {t("gui.cancel")}
          </button>
          <button className="btn primary" disabled={!valid} onClick={() => onOk(v.trim())}>
            OK
          </button>
        </>
      }
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (valid) onOk(v.trim());
        }}
      >
        <label className="form-row">
          <span className="form-label">{label}</span>
          <input
            className="input mono"
            autoFocus
            value={v}
            spellCheck={false}
            onFocus={(e) => {
              // renomear: seleciona o nome sem a extensão
              const dot = e.target.value.lastIndexOf(".");
              e.target.setSelectionRange(0, dot > 0 ? dot : e.target.value.length);
            }}
            onChange={(e) => setV(e.target.value)}
          />
        </label>
      </form>
    </Modal>
  );
}
