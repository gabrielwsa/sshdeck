import type { Host } from "./api";
import { t } from "./i18n";

// Ícones do Nerd Fonts (Font Awesome)
export const KEY_ICON = "\uf023"; // cadeado
export const SSH_ICON = "\uf120"; // terminal
export const RDP_ICON = "\uf17a"; // windows
export const DOT = "\uf444";
export const ICON = {
  search: "\uf002",
  plus: "\uf067",
  close: "\uf00d",
  panel: "\uf0db",
  copy: "\uf0c5",
  keyboard: "\uf11c",
  edit: "\uf040",
  trash: "\uf1f8",
  transfer: "\uf0ec",
  key: "\uf084",
  refresh: "\uf021",
  plug: "\uf1e6",
  power: "\uf011",
  info: "\uf05a",
  ssh: "\uf120",
  rdp: "\uf17a",
  download: "\uf019",
  upload: "\uf093",
  check: "\uf00c",
  question: "\uf128",
  language: "\uf1ab",
  import: "\uf0ed",
  spinner: "\uf110",
};

/** Coluna "Extra": chave do SSH ou domínio/túnel do RDP. */
export function extra(h: Host) {
  if (h.kind === "ssh") return h.identityFile;
  const parts = [];
  if (h.domain) parts.push(t("ui.extra_domain", { d: h.domain }));
  if (h.gateway) parts.push(t("ui.extra_via", { gw: h.gateway }));
  return parts.join(" · ");
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "13/10/2026 ● 14:59:11", no fuso do sistema. */
export function localDatetime(secs: number) {
  const d = new Date(secs * 1000);
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()} ${DOT} ${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** "há 5 min", "há 2 h", "há 3 dia(s)" */
export function ago(then: number) {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - then);
  if (secs < 60) return t("info.ago_now");
  if (secs < 3600) return t("info.ago_min", { n: Math.floor(secs / 60) });
  if (secs < 86400) return t("info.ago_h", { n: Math.floor(secs / 3600) });
  return t("info.ago_d", { n: Math.floor(secs / 86400) });
}

export function elapsed(secs: number) {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${pad(secs % 60)}s`;
  return `${Math.floor(secs / 3600)}h ${pad(Math.floor((secs % 3600) / 60))}m`;
}

/** Kernel sem repetir a arquitetura quando ela já está no nome. */
export function kernelText(i: { kernel: string; arch: string }) {
  return i.kernel.endsWith(i.arch) ? i.kernel : `${i.kernel} (${i.arch})`;
}

export function hardwareText(i: { cpus: number | null; mem_kb: number | null }) {
  return [
    i.cpus != null ? `${i.cpus} CPU` : "",
    i.mem_kb != null ? `${(i.mem_kb / 1024 / 1024).toFixed(1)} GB` : "",
  ]
    .filter(Boolean)
    .join(" · ");
}
