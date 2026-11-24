import { Channel, invoke } from "@tauri-apps/api/core";

export type SysInfo = {
  os: string;
  hostname: string;
  kernel: string;
  arch: string;
  cpus: number | null;
  mem_kb: number | null;
  collected: number;
};

export type Host = {
  alias: string;
  kind: "ssh" | "rdp";
  hostname: string;
  user: string;
  port: number;
  identityFile: string;
  domain: string;
  gateway: string;
  resolution: string;
  notes: string;
  hasPassword: boolean;
  lastUsed: number | null;
  created: number | null;
  updated: number | null;
  sysinfo: SysInfo | null;
  sysinfoAllowed: boolean | null;
};

export type HostForm = {
  kind: "Ssh" | "Rdp";
  alias: string;
  hostname: string;
  user: string;
  port: string;
  identityFile: string;
  password: string;
  notes: string;
  domain: string;
  resolution: string;
  gateway: string;
  sftpPort: string;
  ftpPort: string;
};

export type TransferView = {
  alias: string;
  upload: boolean;
  percent: number;
  bytes: string;
  rate: string;
  eta: string;
};
export type Tick = {
  transfers: TransferView[];
  events: { msg: string }[];
};
export type Suggest = { state: "loading" | "failed" | "ready"; error: string; items: string[] };

export type VaultState = "missing" | "locked" | "unlocked";

export const vaultState = () => invoke<VaultState>("vault_state");
export const vaultUnlock = (password: string) => invoke<void>("vault_unlock", { password });
export const vaultCreate = (password: string) => invoke<void>("vault_create", { password });
export const listHosts = () => invoke<Host[]>("hosts");
export const markUsed = (alias: string) => invoke<Host[]>("mark_used", { alias });
export const copyPassword = (alias: string) => invoke<string>("copy_password", { alias });
export const typePassword = (id: number, alias: string) =>
  invoke<string>("type_password", { id, alias });

export const hostGet = (alias: string) => invoke<HostForm>("host_get", { alias });
export const keyHint = () => invoke<string>("key_hint");
export const hostSave = (editing: string | null, form: HostForm) =>
  invoke<Host[]>("host_save", { editing, form });
export const hostDelete = (alias: string) => invoke<Host[]>("host_delete", { alias });
export const hostsImport = () => invoke<{ added: number; hosts: Host[] }>("hosts_import");
export const sysinfoAllow = (alias: string, allowed: boolean) =>
  invoke<Host[]>("sysinfo_allow", { alias, allowed });
export const sysinfoCollect = (alias: string) => invoke<Host[]>("sysinfo_collect", { alias });
export const vaultRekey = (password: string) => invoke<string>("vault_rekey", { password });
export const langs = () => invoke<[{ code: string; name: string }[], string]>("langs");
export const setLang = (code: string) => invoke<string>("set_lang", { code });
export const rdpTypePassword = (id: number, alias: string) =>
  invoke<string>("rdp_type_password", { id, alias });
export const rdpCertSave = (alias: string, fingerprint: string) =>
  invoke<Host[]>("rdp_cert_save", { alias, fingerprint });
export const transferDefaults = () => invoke<string>("transfer_defaults");
export const transferStart = (alias: string, upload: boolean, remote: string, local: string) =>
  invoke<string>("transfer_start", { alias, upload, remote, local });
export const transferCancel = (alias: string) => invoke<void>("transfer_cancel", { alias });
export const lsOpen = (alias: string) => invoke<number>("ls_open", { alias });
export const lsClose = (id: number) => invoke<void>("ls_close", { id });
export const lsSuggest = (lister: number | null, input: string) =>
  invoke<Suggest>("ls_suggest", { lister, input });
export const tick = () => invoke<Tick>("tick");

/** Abre `ssh alias` num PTY; a saída chega em `onData` e o fim em `onExit`. */
export function sshOpen(
  alias: string,
  kind: "ssh" | "copyid",
  rows: number,
  cols: number,
  onData: (bytes: Uint8Array) => void,
  onExit: () => void,
) {
  const data = new Channel<ArrayBuffer>();
  data.onmessage = (buf) => onData(new Uint8Array(buf));
  const exit = new Channel<null>();
  exit.onmessage = onExit;
  const cmd = kind === "ssh" ? "ssh_open" : "copy_id_open";
  return invoke<number>(cmd, { alias, rows, cols, onData: data, onExit: exit });
}

export const sshWrite = (id: number, data: string) => invoke<void>("ssh_write", { id, data });
export const sshResize = (id: number, rows: number, cols: number) =>
  invoke<void>("ssh_resize", { id, rows, cols });
export const sshClose = (id: number) => invoke<void>("ssh_close", { id });

// --- atualizações (gui/src-tauri/src/update.rs) ---

export type UpdateStatus = {
  state: "idle" | "checking" | "none" | "available" | "downloading" | "ready" | "installing" | "error";
  current: string;
  version: string | null;
  url: string;
  mode: "auto" | "manual";
  needsPassword: boolean;
  percent: number;
  error: string | null;
};

export const updateStatus = () => invoke<UpdateStatus>("update_status");
export const updateCheck = () => invoke<UpdateStatus>("update_check");
export const updateDownload = () => invoke<void>("update_download");
export const updateInstall = () => invoke<void>("update_install");
export const updateOpenPage = () => invoke<void>("update_open_page");

// --- navegador de arquivos (gui/src-tauri/src/files.rs) ---

export type FileProto = "sftp" | "ftp";
export type FileEntry = {
  name: string;
  path: string;
  dir: boolean;
  link: boolean;
  size: number;
  mtime: number | null;
};
export type FileListing = { path: string; entries: FileEntry[]; total: number };
export type FilesProbe = { sftp: boolean; ftp: boolean; sftpPort: number; ftpPort: number };
export type CopyEvent =
  | {
      type: "progress";
      done: number;
      total: number;
      file: string;
      filesDone: number;
      filesTotal: number;
      rate: number;
    }
  | { type: "finished"; error: string | null; cancelled: boolean };

export const filesProbe = (alias: string) => invoke<FilesProbe>("files_probe", { alias });
export const filesOpen = (alias: string, proto: FileProto) =>
  invoke<{ id: number; home: string; port: number }>("files_open", { alias, proto });
export const filesClose = (id: number) => invoke<void>("files_close", { id });
/** `id = null` é o computador local */
export const filesList = (
  id: number | null,
  path: string,
  opts: { limit: number; filter: string; hidden: boolean },
) => invoke<FileListing>("files_list", { id, path, ...opts });
/** quais destes nomes já existem na pasta */
export const filesExisting = (id: number | null, dir: string, names: string[]) =>
  invoke<string[]>("files_existing", { id, dir, names });
export const filesMkdir = (id: number | null, path: string) => invoke<void>("files_mkdir", { id, path });
export const filesRename = (id: number | null, from: string, to: string) =>
  invoke<void>("files_rename", { id, from, to });
export const filesRemove = (id: number | null, items: { path: string; dir: boolean }[]) =>
  invoke<void>("files_remove", { id, items });
export const filesCopy = (
  id: number,
  upload: boolean,
  items: { path: string; dir: boolean }[],
  dest: string,
  onEvent: Channel<CopyEvent>,
) => invoke<number>("files_copy", { id, upload, items, dest, onEvent });
export const filesCancel = (job: number) => invoke<void>("files_cancel", { job });
