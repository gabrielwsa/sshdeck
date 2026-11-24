import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { vaultCreate, vaultUnlock, type VaultState } from "./api";
import { t } from "./i18n";
import { KEY_ICON } from "./ui";

/** Tela da senha mestra: desbloqueia o cofre ou cria um novo (digitando duas vezes). */
export default function Lock({ state, onOpen }: { state: VaultState; onOpen: () => void }) {
  const creating = state === "missing";
  const [input, setInput] = useState("");
  const [first, setFirst] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const field = useRef<HTMLInputElement>(null);

  const title = creating ? t("pw.title_create") : t("pw.title_unlock");
  const prompt = !creating
    ? t("pw.prompt_unlock")
    : first === null
      ? t("pw.prompt_define")
      : t("pw.prompt_confirm");

  useEffect(() => field.current?.focus(), [first, busy]);

  async function submit() {
    const typed = input;
    setInput("");
    if (creating && first === null) {
      if (typed.length < 4) return setError(t("pw.min_len"));
      setError(null);
      return setFirst(typed);
    }
    if (creating && first !== typed) {
      setFirst(null);
      return setError(t("pw.mismatch"));
    }
    setBusy(true);
    try {
      await (creating ? vaultCreate(typed) : vaultUnlock(typed));
      onOpen();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="lock-screen">
      <form
        className="lock-card"
        onSubmit={(e) => {
          e.preventDefault();
          if (!busy) void submit();
        }}
      >
        <div className="logo">{KEY_ICON}</div>
        <h1>{title}</h1>
        <div className="prompt">{prompt}</div>
        <input
          ref={field}
          className="pw-input"
          type="password"
          value={input}
          disabled={busy}
          placeholder={t("gui.master_pw")}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") void getCurrentWindow().close();
          }}
        />
        <div className="msg">
          {error ? (
            <span className="red">{error}</span>
          ) : creating ? (
            <span className="muted">{t("pw.warning")}</span>
          ) : null}
        </div>
        <button className="btn primary wide" type="submit" disabled={busy || !input}>
          {creating ? t("gui.continue") : t("gui.unlock")}
        </button>
      </form>
    </div>
  );
}
