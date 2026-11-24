import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { sshClose, sshOpen, sshResize, sshWrite } from "./api";

// cores do esquema Breeze do Konsole
const THEME = {
  background: "#1e1f22",
  foreground: "#fcfcfc",
  cursor: "#fcfcfc",
  cursorAccent: "#1e1f22",
  selectionBackground: "#3daee966",
  black: "#2b2d31",
  red: "#ed1515",
  green: "#11d116",
  yellow: "#f67400",
  blue: "#1d99f3",
  magenta: "#9b59b6",
  cyan: "#1abc9c",
  white: "#fcfcfc",
  brightBlack: "#7f8c8d",
  brightRed: "#c0392b",
  brightGreen: "#1cdc9a",
  brightYellow: "#fdbc4b",
  brightBlue: "#3daee9",
  brightMagenta: "#8e44ad",
  brightCyan: "#16a085",
  brightWhite: "#ffffff",
};

export type TermHandle = {
  /** id da sessão no Rust (null enquanto o ssh não abriu) */
  id: () => number | null;
  /** rola o histórico em páginas (+ para cima) */
  scroll: (pages: number) => void;
};

type Props = {
  alias: string;
  /** "copyid" roda o ssh-copy-id em vez do ssh */
  kind: "ssh" | "copyid";
  visible: boolean;
  focused: boolean;
  onExit: () => void;
  /** linhas de histórico acima da tela (0 = no fim) */
  onScrollback: (n: number) => void;
  onClick: () => void;
};

/**
 * Um terminal xterm.js ligado a um `ssh alias`. Fica montado mesmo escondido, para a sessão
 * não cair; quando volta a aparecer, reajusta o tamanho.
 */
export default forwardRef<TermHandle, Props>(function Terminal(
  { alias, kind, visible, focused, onExit, onScrollback, onClick },
  ref,
) {
  const box = useRef<HTMLDivElement>(null);
  const term = useRef<XTerm | null>(null);
  const fit = useRef<FitAddon | null>(null);
  const sid = useRef<number | null>(null);
  const cb = useRef({ onExit, onScrollback });
  cb.current = { onExit, onScrollback };

  useImperativeHandle(ref, () => ({
    id: () => sid.current,
    scroll: (pages) => {
      const x = term.current;
      if (!x) return;
      x.scrollLines(-Math.round(pages * Math.max(1, Math.floor(x.rows / 2))));
    },
  }));

  useEffect(() => {
    const xterm = new XTerm({
      fontFamily: '"sshdeck mono", "JetBrainsMono Nerd Font", "JetBrainsMono NF", monospace',
      fontSize: 13.5,
      lineHeight: 1.0,
      cursorBlink: false,
      scrollback: 5000,
      theme: THEME,
      allowProposedApi: true,
    });
    const fitAddon = new FitAddon();
    xterm.loadAddon(fitAddon);
    xterm.open(box.current!);
    fitAddon.fit();
    term.current = xterm;
    fit.current = fitAddon;

    let closed = false;
    const pending: string[] = [];
    sshOpen(
      alias,
      kind,
      xterm.rows,
      xterm.cols,
      (bytes) => xterm.write(bytes),
      () => cb.current.onExit(),
    )
      .then((id) => {
        // o efeito pode ter sido desfeito antes do ssh abrir (sessão fechada)
        if (closed) return void sshClose(id);
        sid.current = id;
        pending.splice(0).forEach((d) => sshWrite(id, d));
        // o tamanho pode ter mudado enquanto o ssh abria
        sshResize(id, xterm.rows, xterm.cols);
      })
      .catch((err) => xterm.write(`\x1b[31m${String(err)}\x1b[0m\r\n`));

    const input = xterm.onData((d) => {
      const id = sid.current;
      if (id === null) pending.push(d);
      else void sshWrite(id, d);
    });
    const resize = xterm.onResize(({ rows, cols }) => {
      const id = sid.current;
      if (id !== null) void sshResize(id, rows, cols);
    });
    const scrolled = xterm.onScroll(() => {
      const b = xterm.buffer.active;
      cb.current.onScrollback(b.baseY - b.viewportY);
    });
    const observer = new ResizeObserver(() => {
      if (box.current?.offsetParent) fitAddon.fit();
    });
    observer.observe(box.current!);

    return () => {
      closed = true;
      observer.disconnect();
      input.dispose();
      resize.dispose();
      scrolled.dispose();
      if (sid.current !== null) void sshClose(sid.current);
      xterm.dispose();
    };
  }, [alias, kind]);

  useEffect(() => {
    if (visible) fit.current?.fit();
  }, [visible]);

  useEffect(() => {
    if (visible && focused) term.current?.focus();
    else term.current?.blur();
  }, [visible, focused]);

  return (
    <div
      className="terminal"
      ref={box}
      style={{ display: visible ? "block" : "none" }}
      onMouseDown={onClick}
    />
  );
});
