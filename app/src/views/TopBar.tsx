import { api } from "../api";
import { exportStl, newProject, openModelsDialog, openProject, openSliced, saveProject } from "../actions";
import { redo, undo } from "../editActions";
import { useStore } from "../store";
import { Button, Icon } from "../ui";
import { RIBBON_TABS } from "./Ribbon";
import { lang, setLang, t as tr } from "../i18n";

export function TopBar({ onPickPrinter }: { onPickPrinter: () => void }) {
  const mode = useStore((s) => s.mode);
  const ribbon = useStore((s) => s.ribbon);
  const setMode = useStore((s) => s.setMode);
  const setRibbon = useStore((s) => s.setRibbon);
  const config = useStore((s) => s.config);
  const setConfig = useStore((s) => s.setConfig);
  const slice = useStore((s) => s.slice);
  const busy = useStore((s) => s.busy);
  const history = useStore((s) => s.history);
  const devices = useStore((s) => s.devices);
  const online = Object.values(devices).filter((d) => d.connected).length;
  const theme = config?.theme ?? "dark";
  return (
    <header className="topbar">
      <div className="brand">
        <img src="/app-icon.png" alt="" width={26} height={26} />
        <span>{tr("OpenResin")}</span>
      </div>
      <div className="row tight">
        <Button icon="undo" variant="ghost" title={tr("Undo (Ctrl+Z)")} disabled={!history.can_undo} onClick={() => void undo()} />
        <Button icon="redo" variant="ghost" title={tr("Redo (Ctrl+Y)")} disabled={!history.can_redo} onClick={() => void redo()} />
        <Button icon="open" variant="ghost" title={tr("Open models (Ctrl+O)")} onClick={() => void openModelsDialog()} />
        <Button icon="save" variant="ghost" title={tr("Save project (Ctrl+S)")} onClick={() => void saveProject()} />
        <details className="menu">
          <summary className="btn ghost" title={tr("More")}>
            <Icon name="file" />
          </summary>
          <div className="menu-items" onClick={(e) => (e.currentTarget.parentElement as HTMLDetailsElement).removeAttribute("open")}>
            <button onClick={() => void newProject()}>{tr("New plate")}</button>
            <button onClick={() => void openProject()}>{tr("Open project…")}</button>
            <button onClick={() => void saveProject()}>{tr("Save project…")}</button>
            <button onClick={() => void openSliced()}>{tr("Open sliced file…")}</button>
            <button onClick={() => void exportStl()}>{tr("Export plate as STL…")}</button>
          </div>
        </details>
      </div>
      <nav className="mode-tabs" aria-label={tr("Workflow")}>
        {RIBBON_TABS.map((t) => (
          <button key={t.id} className={mode === "model" && ribbon === t.id ? "on" : ""} onClick={() => setRibbon(t.id)}>
            <Icon name={t.icon} size={16} />
            {tr(t.label)}
          </button>
        ))}
        <span className="tab-sep" />
        <button className={mode === "preview" ? "on" : ""} disabled={!slice} onClick={() => setMode("preview")}>
          <Icon name="slice" size={16} />
          {tr("Preview")}
        </button>
        <button className={mode === "devices" ? "on" : ""} onClick={() => setMode("devices")}>
          <Icon name="wifi" size={16} />
          {tr("Printers")}
          {online > 0 && <span className="dot" aria-label={tr("{n} connected", { n: online })} />}
        </button>
      </nav>
      <span className="spacer" />
      {busy && <span className="busy small">{busy}</span>}
      <button className="printer-chip" onClick={onPickPrinter} title={tr("Choose printer")}>
        <Icon name="printer" size={16} />
        <span>{config?.printer.name ?? "…"}</span>
      </button>
      <Button icon="help" variant="ghost" title={tr("Keyboard & mouse help (F1)")} onClick={() => window.dispatchEvent(new Event("open-help"))} />
      <button
        className="btn ghost lang-btn"
        title={tr("Language")}
        onClick={() => setLang(lang === "de" ? "en" : "de")}
      >
        {lang === "de" ? "DE" : "EN"}
      </button>
      <Button
        icon={theme === "dark" ? "sun" : "moon"}
        variant="ghost"
        title={tr("Toggle theme")}
        onClick={() => {
          if (!config) return;
          const t = theme === "dark" ? "light" : "dark";
          setConfig({ ...config, theme: t });
          void api.setTheme(t);
        }}
      />
    </header>
  );
}
