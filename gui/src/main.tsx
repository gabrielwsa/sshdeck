import { createRoot } from "react-dom/client";
import App from "./App";
import { loadTranslations } from "./i18n";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

// a fonte embutida precisa estar carregada antes do xterm medir as células
const fonts = Promise.all([
  document.fonts.load('13px "sshdeck mono"'),
  document.fonts.load('bold 13px "sshdeck mono"'),
]).catch(() => {});

// sem StrictMode: ele monta cada terminal duas vezes, e cada montagem abre um ssh
Promise.allSettled([loadTranslations(), fonts]).finally(() => {
  createRoot(document.getElementById("root")!).render(<App />);
});
