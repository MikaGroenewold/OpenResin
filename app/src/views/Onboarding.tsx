import { useMemo, useState } from "react";
import { api, errorText } from "../api";
import { discover, openModelsDialog } from "../actions";
import { lang, setLang, t as tr } from "../i18n";
import { useStore } from "../store";
import { Button, Icon, Modal } from "../ui";

const KEY = "openresin.onboarded";

export function needsOnboarding(): boolean {
  try {
    return localStorage.getItem(KEY) !== "1";
  } catch {
    return false;
  }
}

function markDone() {
  try {
    localStorage.setItem(KEY, "1");
  } catch {
    /* ignore */
  }
}

/** First start: language, printer and network in three short steps. */
export function WelcomeDialog({ onClose }: { onClose: () => void }) {
  const printers = useStore((s) => s.printers);
  const config = useStore((s) => s.config);
  const setConfig = useStore((s) => s.setConfig);
  const toast = useStore((s) => s.toast);
  const [step, setStep] = useState(0);
  const [q, setQ] = useState("");
  const list = useMemo(() => {
    const ql = q.toLowerCase();
    return printers.filter((p) => `${p.vendor} ${p.name}`.toLowerCase().includes(ql)).slice(0, 60);
  }, [printers, q]);

  const finish = () => {
    markDone();
    onClose();
  };

  return (
    <Modal title={tr("Welcome to OpenResin")} onClose={finish} wide>
      <ol className="steps">
        {[tr("Language"), tr("Your printer"), tr("Network")].map((s, i) => (
          <li key={s} className={i === step ? "on" : i < step ? "done" : ""}>
            <span>{i + 1}</span> {s}
          </li>
        ))}
      </ol>

      {step === 0 && (
        <div className="welcome-step">
          <p>{tr("OpenResin prepares your models for resin printing and talks to printers on your local network. It works offline and never sends data anywhere.")}</p>
          <div className="row">
            <Button variant={lang === "en" ? "primary" : "default"} onClick={() => lang !== "en" && setLang("en")}>
              English
            </Button>
            <Button variant={lang === "de" ? "primary" : "default"} onClick={() => lang !== "de" && setLang("de")}>
              Deutsch
            </Button>
          </div>
        </div>
      )}

      {step === 1 && (
        <div className="welcome-step">
          <p>{tr("Which printer do you have? You can change this any time in the top right corner.")}</p>
          <input className="search" autoFocus placeholder={tr("Search printers…")} value={q} onChange={(e) => setQ(e.target.value)} />
          <div className="printer-list compact">
            {list.map((p) => (
              <button
                key={p.id}
                className={`printer-row ${config?.printer.id === p.id ? "on" : ""}`}
                onClick={async () => {
                  try {
                    setConfig(await api.setPrinter(p.id));
                  } catch (e) {
                    toast("error", errorText(e));
                  }
                }}
              >
                <span className="name">{p.name}</span>
                <span className="muted small">
                  {p.resolution_x}×{p.resolution_y} · {p.size_x.toFixed(0)}×{p.size_y.toFixed(0)}×{p.size_z.toFixed(0)} mm
                </span>
                <span className="formats small">{p.formats.map((f) => `.${f}`).join(" ")}</span>
                {p.network === "sdcp" && (
                  <span className="badge net">
                    <Icon name="wifi" size={12} /> {tr("LAN")}
                  </span>
                )}
              </button>
            ))}
          </div>
          {config && (
            <p className="small">
              {tr("Selected")}: <strong>{config.printer.name}</strong>
            </p>
          )}
        </div>
      )}

      {step === 2 && (
        <div className="welcome-step">
          <p>{tr("If your printer is connected to WiFi or Ethernet, OpenResin can find it, send files and show its camera.")}</p>
          <Button
            icon="wifi"
            onClick={() => {
              void discover();
              useStore.getState().setMode("devices");
            }}
          >
            {tr("Search my network now")}
          </Button>
          <p className="muted small">{tr("You can also do this later on the Printers page.")}</p>
        </div>
      )}

      <footer className="row welcome-footer">
        {/* "Back" is taken by the Align panel (rear = "Hinten") */}
        {step > 0 && <Button onClick={() => setStep(step - 1)}>{tr("Previous")}</Button>}
        <span className="spacer" />
        <Button variant="ghost" onClick={finish}>
          {tr("Skip")}
        </Button>
        {step < 2 ? (
          <Button variant="primary" onClick={() => setStep(step + 1)}>
            {tr("Next")}
          </Button>
        ) : (
          <Button
            variant="primary"
            icon="open"
            onClick={() => {
              finish();
              useStore.getState().setMode("model");
              void openModelsDialog();
            }}
          >
            {tr("Open a model")}
          </Button>
        )}
      </footer>
    </Modal>
  );
}

const SHORTCUTS: [string, string][] = [
  ["Ctrl+O", "Open models"],
  ["Ctrl+S", "Save project"],
  ["Ctrl+Z / Ctrl+Y", "Undo / redo"],
  ["Ctrl+A", "Select all models"],
  ["Ctrl+Enter", "Slice"],
  ["Q / W / E / R", "Select / move / rotate / scale"],
  ["M", "Measure"],
  ["← ↑ → ↓", "Move selection by 1 mm (Shift: 10 mm, Alt: 0.1 mm)"],
  ["Del", "Delete selection"],
  ["Esc", "Clear selection, back to select tool"],
  ["? / F1", "This help"],
];

const MOUSE: [string, string][] = [
  ["Left drag on a model", "Move it on the plate"],
  ["Left drag on empty space", "Rotate the view"],
  ["Right drag", "Pan the view"],
  ["Mouse wheel", "Zoom"],
  ["Ctrl+click", "Add to / remove from selection"],
  ["Double-click a name", "Rename"],
];

/** Key names as printed on keyboards of the UI language (Strg/Entf on German ones). */
const keyLabel = (k: string) => k.replace(/\bCtrl\b/g, tr("Ctrl")).replace(/\bDel\b/g, tr("Del"));

export function HelpDialog({ onClose }: { onClose: () => void }) {
  return (
    <Modal title={tr("Keyboard & mouse")} onClose={onClose} wide>
      <div className="help-grid">
        <div>
          <h4>{tr("Keyboard")}</h4>
          <dl className="kv">
            {SHORTCUTS.map(([k, v]) => (
              <div key={k} className="contents">
                <dt>
                  <kbd>{keyLabel(k)}</kbd>
                </dt>
                <dd>{tr(v)}</dd>
              </div>
            ))}
          </dl>
        </div>
        <div>
          <h4>{tr("Mouse")}</h4>
          <dl className="kv">
            {MOUSE.map(([k, v]) => (
              <div key={k} className="contents">
                <dt>{tr(k)}</dt>
                <dd>{tr(v)}</dd>
              </div>
            ))}
          </dl>
          <h4>{tr("Workflow")}</h4>
          <ol className="small workflow">
            <li>{tr("Prepare: open, place and orient your models.")}</li>
            <li>{tr("Support: add supports automatically or by clicking.")}</li>
            <li>{tr("Export: slice, check the layers in Preview.")}</li>
            <li>{tr("Send the file to your printer or save it to a USB stick.")}</li>
          </ol>
        </div>
      </div>
    </Modal>
  );
}
