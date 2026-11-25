import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useRef } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";

/** Eventos da sessão RDP (o Rust manda, ver rdpview.rs). */
export type RdpEvent =
  | { type: "connecting" }
  | { type: "connected" }
  | { type: "certNew"; fingerprint: string }
  | { type: "certChanged"; fingerprint: string }
  | { type: "ended"; error: string };

export type RdpHandle = { id: () => number | null };

type Input =
  | { t: "move"; x: number; y: number }
  | { t: "button"; x: number; y: number; b: number; down: boolean }
  | { t: "wheel"; x: number; y: number; d: number; h: boolean }
  | { t: "key"; c: number; down: boolean }
  | { t: "sync"; caps: boolean; num: boolean; scroll: boolean }
  | { t: "resize"; w: number; h: number };

const send = (id: number, input: Input) => invoke("rdp_input", { id, input }).catch(() => {});

// Todas as abas usam a mesma revisão, pois a ordem de chegada das chamadas IPC pode variar.
let clipboardOwner: number | null = null;
let clipboardRevision = 0;
function setClipboardFocus(id: number, active: boolean) {
  if (active === (clipboardOwner === id)) return;
  clipboardOwner = active ? id : null;
  void invoke("rdp_clipboard_focus", { id: clipboardOwner, revision: ++clipboardRevision }).catch(() => {});
}

// KeyboardEvent.code → scancode do teclado PC (conjunto 1); 0x100 = tecla estendida (E0).
// O Windows aplica o layout da conta, então as teclas físicas (ABNT2, latino...) funcionam.
const SCANCODES: Record<string, number> = {
  Escape: 0x01, Digit1: 0x02, Digit2: 0x03, Digit3: 0x04, Digit4: 0x05, Digit5: 0x06,
  Digit6: 0x07, Digit7: 0x08, Digit8: 0x09, Digit9: 0x0a, Digit0: 0x0b, Minus: 0x0c,
  Equal: 0x0d, Backspace: 0x0e, Tab: 0x0f, KeyQ: 0x10, KeyW: 0x11, KeyE: 0x12, KeyR: 0x13,
  KeyT: 0x14, KeyY: 0x15, KeyU: 0x16, KeyI: 0x17, KeyO: 0x18, KeyP: 0x19, BracketLeft: 0x1a,
  BracketRight: 0x1b, Enter: 0x1c, ControlLeft: 0x1d, KeyA: 0x1e, KeyS: 0x1f, KeyD: 0x20,
  KeyF: 0x21, KeyG: 0x22, KeyH: 0x23, KeyJ: 0x24, KeyK: 0x25, KeyL: 0x26, Semicolon: 0x27,
  Quote: 0x28, Backquote: 0x29, ShiftLeft: 0x2a, Backslash: 0x2b, KeyZ: 0x2c, KeyX: 0x2d,
  KeyC: 0x2e, KeyV: 0x2f, KeyB: 0x30, KeyN: 0x31, KeyM: 0x32, Comma: 0x33, Period: 0x34,
  Slash: 0x35, ShiftRight: 0x36, NumpadMultiply: 0x37, AltLeft: 0x38, Space: 0x39,
  CapsLock: 0x3a, F1: 0x3b, F2: 0x3c, F3: 0x3d, F4: 0x3e, F5: 0x3f, F6: 0x40, F7: 0x41,
  F8: 0x42, F9: 0x43, F10: 0x44, NumLock: 0x45, ScrollLock: 0x46, Numpad7: 0x47,
  Numpad8: 0x48, Numpad9: 0x49, NumpadSubtract: 0x4a, Numpad4: 0x4b, Numpad5: 0x4c,
  Numpad6: 0x4d, NumpadAdd: 0x4e, Numpad1: 0x4f, Numpad2: 0x50, Numpad3: 0x51,
  Numpad0: 0x52, NumpadDecimal: 0x53, IntlBackslash: 0x56, F11: 0x57, F12: 0x58,
  IntlRo: 0x73, NumpadComma: 0x7e,
  NumpadEnter: 0x11c, ControlRight: 0x11d, NumpadDivide: 0x135, PrintScreen: 0x137,
  AltRight: 0x138, Home: 0x147, ArrowUp: 0x148, PageUp: 0x149, ArrowLeft: 0x14b,
  ArrowRight: 0x14d, End: 0x14f, ArrowDown: 0x150, PageDown: 0x151, Insert: 0x152,
  Delete: 0x153, MetaLeft: 0x15b, MetaRight: 0x15c, ContextMenu: 0x15d,
};

/** Resolução fixa pedida no host ("1920x1080"); senão a tela acompanha o tamanho da aba. */
function fixedSize(resolution: string): [number, number] | null {
  const m = /^(\d{3,5})x(\d{3,5})$/i.exec(resolution.trim());
  return m ? [Number(m[1]), Number(m[2])] : null;
}

type Props = {
  alias: string;
  resolution: string;
  visible: boolean;
  focused: boolean;
  onEvent: (e: RdpEvent) => void;
  onClick: () => void;
};

/**
 * Sessão RDP numa aba: a tela do Windows é desenhada num canvas (só os pedaços que mudam),
 * e mouse/teclado vão para o Rust. Fica montada mesmo escondida, como os terminais.
 */
export default forwardRef<RdpHandle, Props>(function RdpTab(
  { alias, resolution, visible, focused, onEvent, onClick },
  ref,
) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const sid = useRef<number | null>(null);
  const cb = useRef(onEvent);
  cb.current = onEvent;
  const pressed = useRef(new Set<number>());
  const needSync = useRef(true);

  useImperativeHandle(ref, () => ({ id: () => sid.current }));

  const active = useRef(false);
  useEffect(() => {
    // Ao voltar de outro app, confere já o que foi copiado lá.
    const onFocus = () => {
      if (sid.current !== null) setClipboardFocus(sid.current, active.current);
    };
    const onBlur = () => {
      if (sid.current !== null) setClipboardFocus(sid.current, false);
    };
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  useLayoutEffect(() => {
    active.current = visible && focused;
    if (sid.current !== null) setClipboardFocus(sid.current, active.current && document.hasFocus());
    return () => {
      active.current = false;
      if (sid.current !== null) setClipboardFocus(sid.current, false);
    };
  }, [visible, focused]);

  useEffect(() => {
    const el = canvas.current!;
    const ctx = el.getContext("2d")!;
    const fixed = fixedSize(resolution);
    const rect = box.current!.getBoundingClientRect();
    // tamanho par: alguns servidores recusam larguras ímpares
    const [w, h] = fixed ?? [Math.max(640, Math.floor(rect.width / 2) * 2), Math.max(480, Math.floor(rect.height / 2) * 2)];

    const frames = new Channel<ArrayBuffer>();
    frames.onmessage = (buf) => {
      const v = new DataView(buf);
      const kind = v.getUint8(0);
      const u16 = (i: number) => v.getUint16(1 + i * 2, true);
      if (kind === 1) {
        const [x, y, rw, rh] = [u16(0), u16(1), u16(2), u16(3)];
        const pixels = new Uint8ClampedArray(buf, 9, rw * rh * 4);
        ctx.putImageData(new ImageData(pixels, rw, rh), x, y);
      } else if (kind === 2) {
        el.width = u16(0);
        el.height = u16(1);
      } else if (kind === 3) {
        const [cw, ch, hx, hy] = [u16(0), u16(1), u16(2), u16(3)];
        if (!cw || !ch) return;
        const c = document.createElement("canvas");
        c.width = cw;
        c.height = ch;
        c.getContext("2d")!.putImageData(
          new ImageData(new Uint8ClampedArray(buf, 9, cw * ch * 4), cw, ch),
          0,
          0,
        );
        el.style.cursor = `url(${c.toDataURL()}) ${hx} ${hy}, default`;
      } else if (kind === 4) {
        el.style.cursor = "default";
      } else if (kind === 5) {
        el.style.cursor = "none";
      }
    };
    const events = new Channel<RdpEvent>();
    events.onmessage = (e) => cb.current(e);

    let closed = false;
    invoke<number>("rdp_open", { alias, width: w, height: h, onFrame: frames, onEvent: events })
      .then((id) => {
        if (closed) return void invoke("rdp_close", { id });
        sid.current = id;
        setClipboardFocus(id, active.current && document.hasFocus());
      })
      .catch((err) => cb.current({ type: "ended", error: String(err) }));

    // a área de trabalho acompanha o tamanho da aba (o Windows redimensiona sozinho)
    let timer = 0;
    const observer = new ResizeObserver(() => {
      if (fixed || !box.current?.offsetParent) return;
      clearTimeout(timer);
      timer = window.setTimeout(() => {
        const r = box.current!.getBoundingClientRect();
        const id = sid.current;
        const nw = Math.floor(r.width / 2) * 2;
        const nh = Math.floor(r.height / 2) * 2;
        if (id !== null && nw >= 200 && nh >= 200 && (nw !== el.width || nh !== el.height))
          void send(id, { t: "resize", w: nw, h: nh });
      }, 400);
    });
    observer.observe(box.current!);

    return () => {
      closed = true;
      observer.disconnect();
      clearTimeout(timer);
      if (sid.current !== null) {
        setClipboardFocus(sid.current, false);
        void invoke("rdp_close", { id: sid.current });
        sid.current = null;
      }
    };
  }, [alias, resolution]);

  useEffect(() => {
    if (!(visible && focused)) {
      canvas.current?.blur();
      return;
    }
    const el = canvas.current;
    el?.focus();
    needSync.current = true;
    // Este efeito roda dentro do mousedown que trocou de aba; o navegador move o foco para o
    // elemento clicado logo depois, e o teclado deixaria de chegar ao Windows. Foca de novo
    // quando o clique terminar.
    const timer = window.setTimeout(() => el?.focus(), 0);
    return () => clearTimeout(timer);
  }, [visible, focused]);

  /** Posição do mouse em pixels da área de trabalho (o canvas é escalado com object-fit). */
  function pos(e: React.MouseEvent | React.WheelEvent) {
    const el = canvas.current!;
    const r = el.getBoundingClientRect();
    const scale = Math.min(r.width / el.width, r.height / el.height) || 1;
    const ox = (r.width - el.width * scale) / 2;
    const oy = (r.height - el.height * scale) / 2;
    const x = Math.round((e.clientX - r.left - ox) / scale);
    const y = Math.round((e.clientY - r.top - oy) / scale);
    return {
      x: Math.max(0, Math.min(el.width - 1, x)),
      y: Math.max(0, Math.min(el.height - 1, y)),
    };
  }

  const id = () => sid.current;
  const moveQueued = useRef(false);
  const lastMove = useRef({ x: 0, y: 0 });

  function key(e: React.KeyboardEvent, down: boolean) {
    const n = id();
    const code = SCANCODES[e.code];
    if (n === null || code === undefined) return;
    e.preventDefault();
    if (needSync.current) {
      needSync.current = false;
      void send(n, {
        t: "sync",
        caps: e.getModifierState("CapsLock"),
        num: e.getModifierState("NumLock"),
        scroll: e.getModifierState("ScrollLock"),
      });
    }
    if (down) pressed.current.add(code);
    else pressed.current.delete(code);
    void send(n, { t: "key", c: code, down });
  }

  return (
    <div
      className="rdp-host"
      ref={box}
      style={{ display: visible ? "flex" : "none" }}
      onMouseDown={onClick}
    >
      <canvas
        ref={canvas}
        className="rdp-canvas"
        tabIndex={0}
        width={1}
        height={1}
        onKeyDown={(e) => key(e, true)}
        onKeyUp={(e) => key(e, false)}
        onBlur={() => {
          // solta o que ficou apertado (ex.: Alt ao trocar de janela)
          const n = id();
          if (n !== null) for (const c of pressed.current) void send(n, { t: "key", c, down: false });
          pressed.current.clear();
        }}
        onContextMenu={(e) => e.preventDefault()}
        onMouseMove={(e) => {
          // um movimento por quadro de tela basta
          lastMove.current = pos(e);
          if (moveQueued.current) return;
          moveQueued.current = true;
          requestAnimationFrame(() => {
            moveQueued.current = false;
            const n = id();
            if (n !== null) void send(n, { t: "move", ...lastMove.current });
          });
        }}
        onMouseDown={(e) => {
          const n = id();
          canvas.current?.focus();
          if (n !== null) void send(n, { t: "button", ...pos(e), b: e.button, down: true });
          e.preventDefault();
        }}
        onMouseUp={(e) => {
          const n = id();
          if (n !== null) void send(n, { t: "button", ...pos(e), b: e.button, down: false });
        }}
        onWheel={(e) => {
          const n = id();
          if (n === null) return;
          const horizontal = Math.abs(e.deltaX) > Math.abs(e.deltaY);
          const delta = horizontal ? e.deltaX : e.deltaY;
          if (!delta) return;
          // um "clique" da roda = 120 no RDP; positivo é para cima (vertical) e para a direita
          const d = horizontal ? (delta > 0 ? 120 : -120) : delta < 0 ? 120 : -120;
          void send(n, { t: "wheel", ...pos(e), d, h: horizontal });
        }}
      />
    </div>
  );
});
