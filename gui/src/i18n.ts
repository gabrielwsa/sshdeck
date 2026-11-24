import { invoke } from "@tauri-apps/api/core";

// As traduções ficam no locales/app.yml (as mesmas usadas pelo Rust).
import yaml from "../../locales/app.yml?raw";

// todas as chaves do arquivo; o Rust devolve o texto no idioma atual
const KEYS = [...yaml.matchAll(/^([a-z_]+\.[a-z0-9_]+):$/gm)].map((m) => m[1]);

let table: Record<string, string> = {};

export async function loadTranslations() {
  table = await invoke<Record<string, string>>("translations", { keys: KEYS });
}

/** `%{nome}` no texto é trocado pelo valor em `args`, como no rust-i18n. */
export function t(key: string, args: Record<string, string | number> = {}) {
  let text = table[key] ?? key;
  for (const [k, v] of Object.entries(args)) text = text.replaceAll(`%{${k}}`, String(v));
  return text;
}
