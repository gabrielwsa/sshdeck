import { useEffect, useRef, useState } from "react";
import { type FileProto, type FilesProbe, filesProbe } from "./api";
import { t } from "./i18n";
import { ICON } from "./ui";

type Choice = "rsync" | FileProto;

/**
 * Botão "Transferir" dos detalhes do host: rsync (popup), SFTP e FTP (navegador de arquivos em
 * aba). Ao abrir, testa as portas do SFTP e do FTP e desabilita o que não responde.
 */
export function TransferMenu({
  alias,
  open,
  rsyncBusy,
  onOpen,
  onClose,
  onPick,
}: {
  alias: string;
  open: boolean;
  rsyncBusy: boolean;
  onOpen: () => void;
  onClose: () => void;
  onPick: (c: Choice) => void;
}) {
  const [probe, setProbe] = useState<FilesProbe | null>(null);
  const [cursor, setCursor] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;

  useEffect(() => {
    if (!open) return;
    setProbe(null);
    setCursor(0);
    let live = true;
    filesProbe(alias).then(
      (p) => live && setProbe(p),
      () => live && setProbe({ sftp: false, ftp: false, sftpPort: 0, ftpPort: 0 }),
    );
    box.current?.focus();
    const away = (e: MouseEvent) => {
      if (!box.current?.parentElement?.contains(e.target as Node)) close.current();
    };
    window.addEventListener("mousedown", away);
    return () => {
      live = false;
      window.removeEventListener("mousedown", away);
    };
  }, [open, alias]);

  const items: { id: Choice; label: string; icon: string; note: string; ok: boolean }[] = [
    {
      id: "rsync",
      label: "rsync",
      icon: ICON.transfer,
      note: rsyncBusy ? t("files.rsync_busy") : t("files.rsync_note"),
      ok: !rsyncBusy,
    },
    {
      id: "sftp",
      label: "SFTP",
      icon: ICON.server,
      note: !probe
        ? t("files.probing")
        : probe.sftp
          ? t("files.port_ok", { port: probe.sftpPort })
          : t("files.port_closed", { port: probe.sftpPort }),
      ok: !!probe?.sftp,
    },
    {
      id: "ftp",
      label: "FTP",
      icon: ICON.globe,
      note: !probe
        ? t("files.probing")
        : probe.ftp
          ? t("files.port_ok", { port: probe.ftpPort })
          : t("files.port_closed", { port: probe.ftpPort }),
      ok: !!probe?.ftp,
    },
  ];

  function onKey(e: React.KeyboardEvent) {
    let handled = true;
    if (e.key === "Escape") onClose();
    else if (e.key === "ArrowDown" || e.key === "j") setCursor((c) => (c + 1) % items.length);
    else if (e.key === "ArrowUp" || e.key === "k") setCursor((c) => (c + items.length - 1) % items.length);
    else if (e.key === "Enter" && items[cursor].ok) onPick(items[cursor].id);
    else if (e.key === "r" && items[0].ok) onPick("rsync");
    else if (e.key === "s" && items[1].ok) onPick("sftp");
    else if (e.key === "f" && items[2].ok) onPick("ftp");
    else handled = false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  return (
    <div className="dropdown">
      <button className={`btn${open ? " on" : ""}`} title={`${t("files.transfer")} (f)`} onClick={open ? onClose : onOpen}>
        <span className="nf">{ICON.transfer}</span> {t("files.transfer")}{" "}
        <span className="nf caret">{ICON.chevronDown}</span>
      </button>
      {open && (
        <div ref={box} className="menu" tabIndex={-1} onKeyDown={onKey}>
          {items.map((it, i) => (
            <button
              key={it.id}
              className={`menu-item${i === cursor ? " cursor" : ""}`}
              disabled={!it.ok}
              onMouseEnter={() => setCursor(i)}
              onClick={() => onPick(it.id)}
            >
              <span className="nf">{it.icon}</span>
              <span className="label">{it.label}</span>
              <span className="note">
                {!probe && it.id !== "rsync" && <span className="nf spin">{ICON.spinner}</span>} {it.note}
              </span>
              <kbd>{it.id === "rsync" ? "r" : it.id === "sftp" ? "s" : "f"}</kbd>
            </button>
          ))}
          {probe && (!probe.sftp || !probe.ftp) && <div className="menu-foot">{t("files.port_hint")}</div>}
        </div>
      )}
    </div>
  );
}
