import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, errorText, type DeviceStatusEvent, type DownloadProgress, type UploadProgress } from "./api";
import { deleteSelection, importPaths, loadSavedDevices, openModelsDialog, saveProject, sliceNow } from "./actions";
import { nudge, redo, refreshHistory, syncScene, undo } from "./editActions";
import { useStore } from "./store";
import { Toasts } from "./ui";
import { DevicesView } from "./views/DevicesView";
import { ModelView } from "./views/ModelView";
import { PrinterPicker } from "./views/PrinterPicker";
import { SettingsDialog } from "./views/SettingsDialog";
import { TopBar } from "./views/TopBar";
import { HelpDialog, WelcomeDialog, needsOnboarding } from "./views/Onboarding";
import { t as tr } from "./i18n";

// files passed on the command line are opened once per process
let startupHandled = false;

export function App() {
  const mode = useStore((s) => s.mode);
  const config = useStore((s) => s.config);
  const [picker, setPicker] = useState(false);
  const [settings, setSettings] = useState(false);
  const [dragOver, setDragOver] = useState(false);
  const [welcome, setWelcome] = useState(needsOnboarding());
  const [help, setHelp] = useState(false);

  useEffect(() => {
    const s = useStore.getState();
    Promise.all([api.getConfig(), api.listPrinters(), api.sceneObjects()])
      .then(async ([cfg, printers, objs]) => {
        s.setConfig(cfg);
        s.setPrinters(printers);
        // after a frontend reload the backend may still hold models
        await syncScene(objs);
        await refreshHistory();
        if (!startupHandled) {
          const flags = await api.startupFlags();
          if (flags.includes("--printers")) {
            useStore.getState().setMode("devices");
            setWelcome(false);
          }
        }
        if (!objs.length && !startupHandled) {
          startupHandled = true;
          const files = await api.startupFiles();
          if (files.length) await importPaths(files);
        }
      })
      .catch((e) => s.toast("error", errorText(e)));
    void loadSavedDevices();

    const unlisten: Promise<() => void>[] = [
      listen<{ done: number; total: number }>("slice-progress", (e) => useStore.getState().setSliceProgress(e.payload)),
      listen<DeviceStatusEvent>("device-status", (e) => {
        const p = e.payload;
        const s = useStore.getState();
        const prev = s.devices[p.ip];
        const was = prev?.status?.status.state;
        const now = p.status.state;
        if (was && was !== now) {
          const name = prev?.name ?? p.ip;
          if (now === "completed" && ["printing", "paused"].includes(was)) s.toast("success", `${name}: ${tr("print finished")} 🎉`);
          if (now === "error") s.toast("error", `${name}: ${tr("printer reports an error")}`);
          if (now === "stopped" && was === "printing") s.toast("info", `${name}: ${tr("print stopped")}`);
          if (!p.connected && prev?.connected) s.toast("error", `${name}: ${tr("connection lost")}`);
        }
        s.updateDevice(p.ip, { status: p, connected: p.connected });
      }),
      listen<DownloadProgress>("download-progress", (e) => {
        const p = e.payload;
        useStore.getState().setDownload(`${p.ip}|${p.path}`, p.done >= p.total && p.total > 0 ? null : p);
      }),
      listen<UploadProgress>("upload-progress", (e) => {
        useStore.getState().updateDevice(e.payload.ip, { upload: e.payload });
      }),
      getCurrentWebview().onDragDropEvent((e) => {
        if (e.payload.type === "over" || e.payload.type === "enter") setDragOver(true);
        else if (e.payload.type === "leave") setDragOver(false);
        else if (e.payload.type === "drop") {
          setDragOver(false);
          const s = useStore.getState();
          if (s.mode === "devices" && s.dropTarget) {
            window.dispatchEvent(new CustomEvent("printer-drop", { detail: e.payload.paths }));
          } else void importPaths(e.payload.paths);
        }
      }),
    ];
    return () => {
      for (const u of unlisten) void u.then((f) => f());
    };
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = config?.theme ?? "dark";
  }, [config?.theme]);

  useEffect(() => {
    const open = () => setSettings(true);
    const openHelp = () => setHelp(true);
    window.addEventListener("open-settings", open);
    window.addEventListener("open-help", openHelp);
    return () => {
      window.removeEventListener("open-settings", open);
      window.removeEventListener("open-help", openHelp);
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      const s = useStore.getState();
      const mod = e.ctrlKey || e.metaKey;
      if (e.key === "F1" || (e.key === "?" && !mod)) {
        e.preventDefault();
        setHelp(true);
        return;
      }
      if (mod && e.key.toLowerCase() === "o") {
        e.preventDefault();
        void openModelsDialog();
      } else if (mod && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void saveProject();
      } else if (mod && e.key.toLowerCase() === "z" && !e.shiftKey) {
        e.preventDefault();
        void undo();
      } else if (mod && (e.key.toLowerCase() === "y" || (e.key.toLowerCase() === "z" && e.shiftKey))) {
        e.preventDefault();
        void redo();
      } else if (mod && e.key.toLowerCase() === "a" && s.mode === "model") {
        e.preventDefault();
        s.select(s.objects.map((o) => o.id));
      } else if (mod && e.key === "Enter") {
        void sliceNow(null);
      } else if (s.mode === "model" && !mod) {
        const map: Record<string, "select" | "move" | "rotate" | "scale" | "measure"> = {
          q: "select",
          w: "move",
          e: "rotate",
          r: "scale",
          m: "measure",
        };
        const t = map[e.key.toLowerCase()];
        if (t) s.setTool(t);
        const step = e.shiftKey ? 10 : e.altKey ? 0.1 : 1;
        if (e.key === "ArrowLeft") void nudge(-step, 0);
        if (e.key === "ArrowRight") void nudge(step, 0);
        if (e.key === "ArrowUp") void nudge(0, step);
        if (e.key === "ArrowDown") void nudge(0, -step);
        if (e.key.startsWith("Arrow")) e.preventDefault();
        if (e.key === "Delete" || e.key === "Backspace") void deleteSelection();
        if (e.key === "Escape") {
          s.select([]);
          s.setTool("select");
          s.setPanel(null);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <TopBar onPickPrinter={() => setPicker(true)} />
      <main className="main">
        <div style={{ display: mode === "model" || mode === "preview" ? "contents" : "none" }}>
          <ModelView />
        </div>
        {mode === "devices" && <DevicesView />}
      </main>
      {dragOver && (
        <div className="drop-overlay">
          {tr(useStore.getState().mode === "devices" && useStore.getState().dropTarget ? "Drop to upload to the printer" : "Drop to add to the plate")}
        </div>
      )}
      {picker && <PrinterPicker onClose={() => setPicker(false)} />}
      {welcome && useStore.getState().printers.length > 0 && <WelcomeDialog onClose={() => setWelcome(false)} />}
      {help && <HelpDialog onClose={() => setHelp(false)} />}
      {settings && config && <SettingsDialog onClose={() => setSettings(false)} />}
      <Toasts />
    </div>
  );
}
