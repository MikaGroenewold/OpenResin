// Small UI primitives and inline icons.
import { useEffect, useState, type ReactNode } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import { useStore } from "./store";
import { t as tr } from "./i18n";

const paths: Record<string, string> = {
  open: "M3 7h6l2 2h10v10H3z M3 7V5h6",
  save: "M5 3h11l3 3v15H5z M8 3v6h8V3 M8 21v-7h8v7",
  plus: "M12 5v14 M5 12h14",
  select: "M5 3l13 8-6 1.5L9 19z",
  move: "M12 2v20 M2 12h20 M12 2l-3 3 M12 2l3 3 M12 22l-3-3 M12 22l3-3 M2 12l3-3 M2 12l3 3 M22 12l-3-3 M22 12l-3 3",
  rotate: "M20 12a8 8 0 1 1-2.3-5.6 M20 4v5h-5",
  scale: "M4 20h7v-7H4z M13 11l7-7 M14 4h6v6",
  supports: "M12 3v8 M7 21h10 M12 11l-4 10 M12 11l4 10 M9 7h6",
  flat: "M3 19h18 M6 15l6-9 6 9z",
  arrange: "M3 3h8v8H3z M13 3h8v8h-8z M3 13h8v8H3z M13 13h8v8h-8z",
  copy: "M8 8h12v12H8z M4 16V4h12",
  trash: "M4 7h16 M9 7V4h6v3 M6 7l1 13h10l1-13",
  slice: "M4 6h16 M4 10h16 M4 14h16 M4 18h16",
  printer: "M6 9V3h12v6 M6 18H4v-7h16v7h-2 M7 14h10v7H7z",
  wifi: "M2 9a15 15 0 0 1 20 0 M5 12.5a10 10 0 0 1 14 0 M8.5 16a5 5 0 0 1 7 0 M12 20h.01",
  camera: "M3 7h4l2-3h6l2 3h4v13H3z M12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8",
  play: "M7 4l13 8-13 8z",
  pause: "M7 4h4v16H7z M13 4h4v16h-4z",
  stop: "M6 6h12v12H6z",
  upload: "M12 16V4 M7 9l5-5 5 5 M4 20h16",
  refresh: "M20 12a8 8 0 1 1-2.3-5.6 M20 4v5h-5",
  settings:
    "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6 M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
  close: "M6 6l12 12 M18 6L6 18",
  file: "M6 2h9l5 5v15H6z M14 2v6h6",
  cube: "M12 2l9 5v10l-9 5-9-5V7z M12 22V12 M21 7l-9 5-9-5",
  eye: "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6",
  sun: "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10 M12 1v2 M12 21v2 M4.2 4.2l1.4 1.4 M18.4 18.4l1.4 1.4 M1 12h2 M21 12h2 M4.2 19.8l1.4-1.4 M18.4 5.6l1.4-1.4",
  moon: "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z",
  mirror: "M12 3v18 M8 7l-5 5 5 5z M16 7l5 5-5 5z",
  link: "M10 14a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1 M14 10a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1",
  unlink: "M17 7l-1 1 M7 17l1-1 M3 3l18 18 M10 14a5 5 0 0 0 7 0 M14 10a5 5 0 0 0-7 0",
  download: "M12 4v12 M7 11l5 5 5-5 M4 20h16",
  undo: "M9 14L4 9l5-5 M4 9h10a6 6 0 0 1 0 12h-3",
  redo: "M15 14l5-5-5-5 M20 9H10a6 6 0 0 0 0 12h3",
  measure: "M3 17L17 3l4 4L7 21z M7 13l2 2 M10 10l2 2 M13 7l2 2",
  repair: "M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z",
  detect: "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16 M21 21l-4.3-4.3 M11 7v4 M11 15h.01",
  orient: "M12 3a9 9 0 1 0 9 9 M12 3v4 M21 12h-4 M12 12l5-5 M17 3h4v4",
  layout: "M3 3h7v7H3z M14 3h7v4h-7z M14 10h7v11h-7z M3 13h7v8H3z",
  hollow: "M12 3c5 0 8 2 8 4v10c0 2-3 4-8 4s-8-2-8-4V7c0-2 3-4 8-4z M12 7c2.8 0 4-.9 4-1.5S14.8 4 12 4 8 4.9 8 5.5 9.2 7 12 7z",
  drill: "M12 2v4 M8 6h8l-1 10h-6z M12 16v6 M10 22h4",
  cut: "M6 4l12 16 M18 4L6 20 M3 12h18",
  split: "M4 4h6v6H4z M14 14h6v6h-6z M10 7h4v10",
  boolean: "M9 15a6 6 0 1 0 0-12 6 6 0 0 0 0 12z M15 21a6 6 0 1 0 0-12 6 6 0 0 0 0 12z",
  label: "M4 7V4h16v3 M9 20h6 M12 4v16",
  combine: "M4 4h7v7H4z M13 13h7v7h-7z M11 7.5h5.5V13",
  align: "M4 3v18 M8 6h12v4H8z M8 14h7v4H8z",
  platform: "M3 18h18v3H3z M7 18V9l5-5 5 5v9",
  edit_support: "M12 3v10 M8 21h8 M12 13l-3 8 M12 13l3 8 M16 4l4 4-6 6",
  manual_support: "M12 7v6 M8 21h8 M12 13l-3 8 M12 13l3 8 M17 3v6 M14 6h6",
  auto_support: "M7 3v6 M17 3v6 M5 21h4 M15 21h4 M7 9l-1 12 M17 9l1 12 M9 5h6",
  magic: "M15 4V2 M15 16v-2 M8 9h2 M20 9h2 M17.8 11.8L19 13 M15 9h.01 M17.8 6.2L19 5 M3 21l9-9 M12.2 6.2L11 5",
  raft: "M3 17h18 M5 21h14l2-4H3z M8 17V9 M16 17V9 M8 9l4-5 4 5",
  overhang: "M4 20h16 M4 20V8h10 M14 8l6 6 M10 12h.01",
  bracing: "M6 3v18 M18 3v18 M6 7l12 6 M18 7L6 13 M6 13l12 6",
  export_model: "M6 2h9l5 5v15H6z M12 18v-7 M9 14l3 3 3-3",
  help: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18 M9.1 9a3 3 0 0 1 5.8 1c0 2-3 3-3 3 M12 17h.01",
  fill: "M3 3h5v5H3z M10 3h5v5h-5z M17 3h4v5h-4z M3 10h5v5H3z M10 10h5v5h-5z M17 10h4v5h-4z M3 17h5v4H3z M10 17h5v4h-5z M17 17h4v4h-4z",
};

export function Icon({ name, size = 18 }: { name: keyof typeof paths | string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name] ?? ""} />
    </svg>
  );
}

export function Button(props: {
  children?: ReactNode;
  icon?: string;
  onClick?: () => void;
  variant?: "primary" | "ghost" | "danger" | "default";
  disabled?: boolean;
  title?: string;
  active?: boolean;
  small?: boolean;
  type?: "button" | "submit";
}) {
  const cls = ["btn", props.variant ?? "default", props.active ? "active" : "", props.small ? "small" : ""].join(" ");
  return (
    <button
      type={props.type ?? "button"}
      className={cls}
      onClick={props.onClick}
      disabled={props.disabled}
      title={props.title}
      // icon-only buttons are named by their tooltip; buttons with text keep the visible text as name
      aria-label={props.children ? undefined : props.title}
    >
      {props.icon && <Icon name={props.icon} size={props.small ? 15 : 18} />}
      {props.children && <span>{props.children}</span>}
    </button>
  );
}

export function NumberField(props: {
  label: string;
  value: number;
  onChange: (v: number) => void;
  step?: number;
  min?: number;
  max?: number;
  unit?: string;
  digits?: number;
  disabled?: boolean;
}) {
  const [text, setText] = useState(String(props.value));
  useEffect(() => {
    setText(props.digits != null ? props.value.toFixed(props.digits) : String(+props.value.toFixed(6)));
  }, [props.value, props.digits]);
  const commit = () => {
    const v = parseFloat(text.replace(",", "."));
    if (!Number.isFinite(v)) {
      setText(String(props.value));
      return;
    }
    let c = v;
    if (props.min != null) c = Math.max(props.min, c);
    if (props.max != null) c = Math.min(props.max, c);
    if (c !== props.value) props.onChange(c);
    else setText(String(props.value));
  };
  return (
    <label className="field">
      <span className="field-label">{props.label}</span>
      <span className="field-input">
        <input
          type="text"
          inputMode="decimal"
          value={text}
          disabled={props.disabled}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            if (e.key === "ArrowUp" || e.key === "ArrowDown") {
              e.preventDefault();
              const step = (props.step ?? 1) * (e.key === "ArrowUp" ? 1 : -1);
              let v = +(props.value + step).toFixed(6);
              if (props.min != null) v = Math.max(props.min, v);
              if (props.max != null) v = Math.min(props.max, v);
              props.onChange(v);
            }
          }}
        />
        {props.unit && <span className="unit">{props.unit}</span>}
      </span>
    </label>
  );
}

export function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  return (
    <label className="toggle">
      <input
        type="checkbox"
        checked={props.checked}
        disabled={props.disabled}
        onChange={(e) => props.onChange(e.target.checked)}
      />
      <span className="toggle-track" />
      <span>{props.label}</span>
    </label>
  );
}

export function Modal(props: { title: string; onClose: () => void; children: ReactNode; wide?: boolean; footer?: ReactNode }) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && props.onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [props]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div className={`modal ${props.wide ? "wide" : ""}`} role="dialog" aria-modal="true" aria-label={props.title}>
        <header className="modal-header">
          <h2>{props.title}</h2>
          <Button icon="close" variant="ghost" title={tr("Close")} onClick={props.onClose} />
        </header>
        <div className="modal-body">{props.children}</div>
        {props.footer && <footer className="modal-footer">{props.footer}</footer>}
      </div>
    </div>
  );
}

export function Progress({ value, label }: { value: number; label?: string }) {
  return (
    <div className="progress" role="progressbar" aria-valuenow={Math.round(value * 100)} aria-valuemin={0} aria-valuemax={100}>
      <div className="progress-fill" style={{ width: `${Math.max(0, Math.min(1, value)) * 100}%` }} />
      {label && <span className="progress-label">{label}</span>}
    </div>
  );
}

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);
  return (
    <div className="toasts" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`} onClick={() => dismiss(t.id)}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

/** Native yes/no question (works on every platform's webview). */
export async function confirmDialog(text: string, title = "OpenResin"): Promise<boolean> {
  try {
    return await ask(text, { title, kind: "warning" });
  } catch {
    return window.confirm(text);
  }
}
