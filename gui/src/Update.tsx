import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { type UpdateStatus, updateDownload, updateStatus } from "./api";
import { t } from "./i18n";
import { ICON } from "./ui";

/** Estado do atualizador, sincronizado com o Rust (evento "update-status"). */
export function useUpdate(): UpdateStatus | null {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    updateStatus().then(setStatus, () => {});
    const off = listen<UpdateStatus>("update-status", (e) => setStatus(e.payload));
    return () => void off.then((f) => f());
  }, []);
  return status;
}

/** Aviso na barra de status: nova versão → baixando → reiniciar e atualizar. */
export function UpdatePill({ s, onRestart }: { s: UpdateStatus | null; onRestart: () => void }) {
  if (!s || !["available", "downloading", "ready", "installing"].includes(s.state)) return null;
  const v = s.version ?? "";

  if (s.state === "downloading")
    return (
      <span className="upd-pill busy">
        <span className="nf spin">{ICON.spinner}</span> {t("gui.upd_downloading", { p: s.percent })}
      </span>
    );
  if (s.state === "installing")
    return (
      <span className="upd-pill busy">
        <span className="nf spin">{ICON.spinner}</span>{" "}
        {s.needsPassword ? t("gui.upd_installing_pw") : t("gui.upd_installing")}
      </span>
    );
  if (s.state === "ready") {
    // instalação cancelada ou que falhou fica aqui, com o motivo na dica
    const hint = s.needsPassword ? t("gui.upd_hint_ready_pw") : t("gui.upd_hint_ready");
    return (
      <button
        className={`upd-pill ${s.error ? "bad" : "ready"}`}
        title={s.error ? `${s.error} · ${hint}` : hint}
        onClick={onRestart}
      >
        <span className="nf">{ICON.refresh}</span> {t("gui.upd_ready", { v })}
      </button>
    );
  }
  // download que falhou: o motivo na dica, o clique tenta de novo
  if (s.error)
    return (
      <button
        className="upd-pill bad"
        title={t("gui.upd_failed", { e: s.error })}
        onClick={() => void updateDownload()}
      >
        <span className="nf">{ICON.download}</span> {t("gui.upd_retry", { v })}
      </button>
    );
  return (
    <button
      className="upd-pill"
      title={s.mode === "auto" ? t("gui.upd_hint_auto") : t("gui.upd_hint_manual")}
      onClick={() => void updateDownload()}
    >
      <span className="nf">{ICON.download}</span>{" "}
      {s.mode === "auto" ? t("gui.upd_available", { v }) : t("gui.upd_manual", { v })}
    </button>
  );
}
