import { createRoot } from "react-dom/client";
import App from "./App";
import { loadTranslations } from "./i18n";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

// sem StrictMode: ele monta cada terminal duas vezes, e cada montagem abre um ssh
loadTranslations().finally(() => {
  createRoot(document.getElementById("root")!).render(<App />);
});
