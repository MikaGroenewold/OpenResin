import { useEffect, useMemo, useRef, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, errorText, type FileEntry } from "../api";
import { connectDevice, discover, loadSavedDevices, run } from "../actions";
import { basename, fmtBytes, fmtDuration } from "../format";
import { t as tr } from "../i18n";
import { useStore, type DeviceEntry } from "../store";
import { Button, Icon, Progress, confirmDialog } from "../ui";

const STATE_LABEL: Record<string, string> = {
  unknown: "Unknown",
  offline: "Offline",
  idle: "Idle",
  printing: "Printing",
  pausing: "Pausing…",
  paused: "Paused",
  stopping: "Stopping…",
  stopped: "Stopped",
  completed: "Completed",
  file_transfer: "Receiving file",
  homing: "Homing",
  file_checking: "Checking file",
  exposure_test: "Exposure test",
  self_test: "Self test",
  error: "Error",
};

/** Translated name of a machine state (the raw id for states the UI does not know yet). */
export const stateLabel = (state: string) => tr(STATE_LABEL[state] ?? state);

const BUSY = ["printing", "paused", "pausing", "stopping", "file_transfer", "homing", "file_checking"];

export const SLICED_EXTS = [
  "goo", "ctb", "cbddlp", "sl1", "zip", "pwmx", "pwma", "pwmb", "pwmo", "pwms", "pmsq", "pm3", "pm3m", "pm3n",
  "pm3r", "px6s", "pm4n", "pm5", "pm5s", "dlp", "cxdlp", "cxdlpv4", "prz",
];

function stateOf(d: DeviceEntry): string {
  return d.status?.status.state ?? (d.connected ? "unknown" : "offline");
}

/** Network printer manager: list on the left, details/files/history on the right. */
export function DevicesView() {
  const devices = useStore((s) => s.devices);
  const selected = useStore((s) => s.selectedDevice);
  const select = useStore((s) => s.selectDevice);
  const [addIp, setAddIp] = useState("");
  const list = Object.values(devices).sort(
    (a, b) => Number(b.connected) - Number(a.connected) || Number(b.saved) - Number(a.saved) || a.name.localeCompare(b.name),
  );

  useEffect(() => {
    void loadSavedDevices();
  }, []);

  useEffect(() => {
    if (!selected && list.length) select(list[0].ip);
  }, [selected, list, select]);

  const add = async () => {
    const ip = addIp.trim();
    if (!ip) return;
    await run(null, async () => {
      await api.saveDevice({ ip, name: ip });
      useStore.getState().updateDevice(ip, { saved: true, name: ip });
      setAddIp("");
      select(ip);
      if (await connectDevice(ip)) {
        const d = useStore.getState().devices[ip];
        const attrs = (d.status?.attributes ?? {}) as Record<string, string>;
        const name = attrs.Name || ip;
        await api.saveDevice({ ip, name, model: attrs.MachineName ?? null });
        useStore.getState().updateDevice(ip, { name, model: attrs.MachineName });
      }
    });
  };

  const current = selected ? devices[selected] : undefined;

  return (
    <div className="devices2">
      <aside className="device-sidebar">
        <header>
          <h2>{tr("Printers")}</h2>
          <Button small icon="wifi" variant="primary" onClick={() => void discover()}>
            {tr("Search")}
          </Button>
        </header>
        <div className="add-ip">
          <input
            placeholder={tr("Add by IP address")}
            value={addIp}
            onChange={(e) => setAddIp(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void add()}
            aria-label={tr("Printer IP address")}
          />
          <Button small icon="plus" title={tr("Add")} onClick={() => void add()} disabled={!addIp.trim()} />
        </div>
        <ul className="device-items">
          {list.map((d) => {
            const st = d.status?.status;
            const state = stateOf(d);
            const p = st?.current_layer != null && st?.total_layers ? st.current_layer / st.total_layers : null;
            return (
              <li key={d.ip} className={selected === d.ip ? "on" : ""} onClick={() => select(d.ip)}>
                <div className="row">
                  <Icon name="printer" size={16} />
                  <span className="name">{d.name}</span>
                  <span className={`state-dot ${state}`} title={stateLabel(state)} />
                </div>
                <span className="muted small">
                  {d.model ?? d.discovered?.model ?? tr("Printer")} · {d.ip}
                </span>
                {p != null && BUSY.includes(state) && (
                  <div className="mini-progress">
                    <div style={{ width: `${p * 100}%` }} />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
        {list.length === 0 && (
          <p className="muted small pad">{tr("No printers yet. Search the network or add a printer by its IP address.")}</p>
        )}
        <p className="muted small pad privacy-note">
          <Icon name="link" size={13} /> {tr("Local network only – nothing leaves your network.")}
        </p>
      </aside>
      <section className="device-detail">
        {current ? (
          <DeviceDetail key={current.ip} d={current} />
        ) : (
          <div className="empty-view">
            <Icon name="wifi" size={44} />
            <h3>{tr("Connect your printer")}</h3>
            <p>{tr("Make sure the printer is switched on and in the same network, then search for it.")}</p>
            <Button variant="primary" icon="wifi" onClick={() => void discover()}>
              {tr("Search network")}
            </Button>
          </div>
        )}
      </section>
    </div>
  );
}

function DeviceDetail({ d }: { d: DeviceEntry }) {
  const update = useStore((s) => s.updateDevice);
  const toast = useStore((s) => s.toast);
  const tab = useStore((s) => s.deviceTab);
  const setTab = useStore((s) => s.setDeviceTab);
  const [renaming, setRenaming] = useState(false);
  const protocol = d.status?.protocol ?? d.discovered?.protocol;
  const legacy = protocol === "chitu-udp";
  const state = stateOf(d);

  // selecting a printer connects to it
  useEffect(() => {
    if (!d.connected && !d.connecting && !d.error) void connectDevice(d.ip);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [d.ip]);

  const tabs = legacy ? (["overview", "files"] as const) : (["overview", "files", "history"] as const);

  return (
    <>
      <header className="detail-header">
        <div className="device-title">
          <Icon name="printer" size={26} />
          <div>
            {renaming ? (
              <input
                className="rename-input"
                autoFocus
                defaultValue={d.name}
                aria-label={tr("Printer name")}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                  if (e.key === "Escape") setRenaming(false);
                }}
                onBlur={(e) => {
                  setRenaming(false);
                  const name = e.target.value.trim();
                  if (!name || name === d.name) return;
                  api
                    .printerRename(d.ip, name)
                    .then(() => update(d.ip, { name }))
                    .catch((err) => toast("error", errorText(err)));
                }}
              />
            ) : (
              <h2
                title={d.connected && !legacy ? tr("Double-click to rename") : undefined}
                onDoubleClick={() => d.connected && !legacy && setRenaming(true)}
              >
                {d.name}
              </h2>
            )}
            <p className="muted small">
              {(d.status?.attributes?.MachineName as string) ?? d.model ?? d.discovered?.model ?? tr("Printer")} · {d.ip}
              {protocol === "sdcp" && d.discovered?.protocol_version ? ` · SDCP ${d.discovered.protocol_version}` : ""}
              {legacy ? ` · ${tr("legacy WiFi (experimental)")}` : ""}
            </p>
          </div>
        </div>
        <span className={`state-pill big ${state}`}>{stateLabel(state)}</span>
        <span className="spacer" />
        {d.connected ? (
          <Button
            icon="unlink"
            variant="ghost"
            onClick={() =>
              void api
                .disconnectPrinter(d.ip)
                .then(() => update(d.ip, { connected: false, camera: null, status: undefined, error: null }))
            }
          >
            {tr("Disconnect")}
          </Button>
        ) : (
          <Button variant="primary" icon="link" disabled={d.connecting} onClick={() => void connectDevice(d.ip)}>
            {tr(d.connecting ? "Connecting…" : "Connect")}
          </Button>
        )}
        {d.saved ? (
          <Button
            small
            variant="ghost"
            onClick={() =>
              void api.forgetDevice(d.ip).then(() => {
                if (d.discovered) update(d.ip, { saved: false, connected: false, camera: null });
                else {
                  useStore.getState().removeDevice(d.ip);
                  useStore.getState().selectDevice(null);
                }
              })
            }
          >
            {tr("Forget")}
          </Button>
        ) : (
          <Button
            small
            variant="ghost"
            onClick={() =>
              void api
                .saveDevice({ ip: d.ip, name: d.name, model: d.model ?? d.discovered?.model ?? null })
                .then(() => update(d.ip, { saved: true }))
            }
          >
            {tr("Remember")}
          </Button>
        )}
      </header>

      {d.error && !d.connected && (
        <div className="callout error">
          <strong>{tr("Could not connect.")}</strong> {d.error}
          <p className="small">
            {tr("Check that the printer is on, connected to the same network, and that the IP address is correct.")}
          </p>
        </div>
      )}
      {legacy && (
        <div className="callout warn small">
          {tr("This printer uses the legacy Chitu WiFi protocol. Support is experimental: upload, start, pause and stop.")}
        </div>
      )}

      <nav className="tabs detail-tabs">
        {tabs.map((t) => (
          <button key={t} className={tab === t ? "on" : ""} onClick={() => setTab(t)}>
            {tr(t === "overview" ? "Overview" : t === "files" ? "Files" : "History")}
          </button>
        ))}
      </nav>
      <div className="detail-body">
        {!d.connected ? (
          <p className="muted pad">{d.connecting ? tr("Connecting…") : tr("Not connected.")}</p>
        ) : tab === "files" ? (
          <FileManager d={d} legacy={legacy} />
        ) : tab === "history" && !legacy ? (
          <HistoryTable d={d} />
        ) : (
          <Overview d={d} legacy={legacy} />
        )}
      </div>
    </>
  );
}

function Overview({ d, legacy }: { d: DeviceEntry; legacy: boolean }) {
  const toast = useStore((s) => s.toast);
  const update = useStore((s) => s.updateDevice);
  const st = d.status?.status;
  const attrs = (d.status?.attributes ?? {}) as Record<string, unknown>;
  const state = stateOf(d);
  const progress = st?.current_layer != null && st?.total_layers ? st.current_layer / st.total_layers : null;
  const remaining = st?.total_s != null && st?.elapsed_s != null ? Math.max(0, st.total_s - st.elapsed_s) : null;
  const eta = remaining != null && BUSY.includes(state) ? new Date(Date.now() + remaining * 1000) : null;

  const command = async (c: "pause" | "resume" | "stop") => {
    if (c === "stop" && !(await confirmDialog(tr("Stop the current print? This cannot be undone.")))) return;
    try {
      await api.printerCommand(d.ip, c);
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  const toggleCamera = async () => {
    if (d.camera) {
      update(d.ip, { camera: null });
      await api.cameraStop(d.ip).catch(() => {});
      return;
    }
    try {
      update(d.ip, { camera: await api.cameraStart(d.ip) });
    } catch (e) {
      toast("error", `${tr("Camera")}: ${errorText(e)}`);
    }
  };

  const snapshot = async () => {
    const p = await save({ defaultPath: `${d.name}-snapshot.jpg`, filters: [{ name: "JPEG", extensions: ["jpg"] }] });
    if (p) await run(null, () => api.cameraSnapshot(d.ip, p));
  };

  const hasCamera = !!d.status?.camera && (st?.camera_connected ?? true);

  return (
    <div className="overview">
      <div className="status-card">
        {st?.filename && !BUSY.includes(state) && <span className="muted small card-caption">{tr("Last print")}</span>}
        {st?.filename ? (
          <p className="filename" title={st.filename}>
            <Icon name="file" size={15} /> {basename(st.filename)}
          </p>
        ) : (
          <p className="muted">{tr(state === "idle" ? "Ready to print." : STATE_LABEL[state] ?? state)}</p>
        )}
        {progress != null && (
          <>
            <div className="big-progress">
              <div className="big-progress-fill" style={{ width: `${progress * 100}%` }} />
              <span>{Math.round(progress * 100)} %</span>
            </div>
            <div className="stat-row">
              <div>
                <span className="muted small">{tr("Layer")}</span>
                <strong>
                  {st?.current_layer} / {st?.total_layers}
                </strong>
              </div>
              {st?.elapsed_s != null && (
                <div>
                  <span className="muted small">{tr("Elapsed")}</span>
                  <strong>{fmtDuration(st.elapsed_s)}</strong>
                </div>
              )}
              {remaining != null && BUSY.includes(state) && (
                <div>
                  <span className="muted small">{tr("Remaining")}</span>
                  <strong>{fmtDuration(remaining)}</strong>
                </div>
              )}
              {eta && (
                <div>
                  <span className="muted small">{tr("Finishes at")}</span>
                  <strong>{eta.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</strong>
                </div>
              )}
            </div>
          </>
        )}
        <div className="row wrap controls">
          {state === "printing" || state === "pausing" ? (
            <Button icon="pause" onClick={() => void command("pause")}>
              {tr("Pause")}
            </Button>
          ) : state === "paused" ? (
            <Button icon="play" variant="primary" onClick={() => void command("resume")}>
              {tr("Resume")}
            </Button>
          ) : null}
          {["printing", "paused", "pausing"].includes(state) && (
            <Button icon="stop" variant="danger" onClick={() => void command("stop")}>
              {tr("Stop")}
            </Button>
          )}
          {!legacy && st?.time_lapse != null && (
            <Button
              icon="camera"
              variant="ghost"
              active={!!st.time_lapse}
              title={tr("Time-lapse recording")}
              onClick={() => void api.printerSetTimeLapse(d.ip, !st.time_lapse).catch((e) => toast("error", errorText(e)))}
            >
              {tr("Time-lapse")} {st.time_lapse ? tr("on") : tr("off")}
            </Button>
          )}
        </div>
        <dl className="kv compact">
          {st?.uv_led_temp != null && (
            <>
              <dt>{tr("UV LED")}</dt>
              <dd>{st.uv_led_temp.toFixed(1)} °C</dd>
            </>
          )}
          {st?.box_temp != null && st.box_temp > 0 && (
            <>
              <dt>{tr("Enclosure")}</dt>
              <dd>{st.box_temp.toFixed(1)} °C</dd>
            </>
          )}
          {st?.free_storage != null && (
            <>
              <dt>{tr("Free storage")}</dt>
              <dd>{fmtBytes(st.free_storage)}</dd>
            </>
          )}
          {attrs.Resolution != null && (
            <>
              <dt>{tr("Resolution")}</dt>
              <dd>{String(attrs.Resolution)}</dd>
            </>
          )}
          {attrs.FirmwareVersion != null && (
            <>
              <dt>{tr("Firmware")}</dt>
              <dd>{String(attrs.FirmwareVersion)}</dd>
            </>
          )}
          {attrs.NetworkStatus != null && (
            <>
              <dt>{tr("Network")}</dt>
              <dd>{tr(String(attrs.NetworkStatus) === "wlan" ? "WiFi" : "Ethernet")}</dd>
            </>
          )}
        </dl>
      </div>
      {hasCamera && (
        <div className="camera-card">
          {d.camera ? (
            <div className="camera">
              <img src={d.camera.stream_url} alt={`${tr("Camera")} ${d.name}`} />
              <div className="camera-actions">
                <Button small icon="save" title={tr("Save snapshot")} onClick={() => void snapshot()} />
                <Button small icon="close" title={tr("Stop camera")} onClick={() => void toggleCamera()} />
              </div>
            </div>
          ) : (
            <button className="camera-placeholder" onClick={() => void toggleCamera()}>
              <Icon name="camera" size={32} />
              <span>{tr("Show camera")}</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
}

interface UploadItem {
  name: string;
  path: string;
  state: "queued" | "uploading" | "done" | "error";
  error?: string;
}

/** Remote file manager for one printer. */
function FileManager({ d, legacy }: { d: DeviceEntry; legacy: boolean }) {
  const toast = useStore((s) => s.toast);
  const downloads = useStore((s) => s.downloads);
  const setDropTarget = useStore((s) => s.setDropTarget);
  const [root, setRoot] = useState<"/local" | "/usb">("/local");
  const [dir, setDir] = useState("/local");
  const [list, setList] = useState<FileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [view, setView] = useState<"list" | "grid">("grid");
  const [queue, setQueue] = useState<UploadItem[]>([]);
  const queueRef = useRef(queue);
  queueRef.current = queue;
  const busy = BUSY.includes(stateOf(d));
  const canDownload = !!d.status?.downloads && root === "/local";

  const load = async (path = dir) => {
    setList(null);
    setError(null);
    try {
      const l = await api.printerFiles(d.ip, path);
      l.sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.path.localeCompare(b.path));
      setList(l);
    } catch (e) {
      setError(root === "/usb" ? tr("No USB drive inserted.") : errorText(e));
      setList([]);
    }
  };
  useEffect(() => {
    void load(dir);
    setChecked(new Set());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dir]);

  const runQueue = async () => {
    for (;;) {
      const next = queueRef.current.find((q) => q.state === "queued");
      if (!next) break;
      queueRef.current = queueRef.current.map((x) => (x.path === next.path ? { ...x, state: "uploading" } : x));
      setQueue(queueRef.current);
      try {
        await api.uploadToPrinter(d.ip, false, next.name, next.path);
        queueRef.current = queueRef.current.map((x) => (x.path === next.path ? { ...x, state: "done" } : x));
      } catch (e) {
        queueRef.current = queueRef.current.map((x) =>
          x.path === next.path ? { ...x, state: "error", error: errorText(e) } : x,
        );
      }
      setQueue(queueRef.current);
    }
    void load("/local");
  };

  const enqueue = (paths: string[]) => {
    if (!paths.length) {
      toast("info", tr("Only sliced printer files can be uploaded."));
      return;
    }
    const items = paths.map((p) => ({ name: basename(p), path: p, state: "queued" as const }));
    const wasIdle = !queueRef.current.some((q) => q.state === "uploading" || q.state === "queued");
    queueRef.current = [...queueRef.current.filter((q) => q.state !== "done"), ...items];
    setQueue(queueRef.current);
    if (wasIdle) void runQueue();
  };

  // files dropped on the window while this view is open are uploaded here
  useEffect(() => {
    setDropTarget(d.ip);
    const onDrop = (e: Event) => {
      const paths = (e as CustomEvent<string[]>).detail;
      enqueue(paths.filter((p) => SLICED_EXTS.includes(p.split(".").pop()?.toLowerCase() ?? "")));
    };
    window.addEventListener("printer-drop", onDrop);
    return () => {
      setDropTarget(null);
      window.removeEventListener("printer-drop", onDrop);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [d.ip]);

  const pickUpload = async () => {
    const sel = await open({ multiple: true, filters: [{ name: tr("Printer files"), extensions: SLICED_EXTS }] });
    if (sel) enqueue(Array.isArray(sel) ? sel : [sel]);
  };

  const download = async (f: FileEntry) => {
    const p = await save({ defaultPath: basename(f.path) });
    if (!p) return;
    try {
      await api.printerDownload(d.ip, f.path, p);
      toast("success", tr("Downloaded {file}", { file: basename(f.path) }));
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  const remove = async (paths: string[]) => {
    if (!paths.length) return;
    const msg =
      paths.length === 1
        ? tr("Delete {file}?", { file: basename(paths[0]) })
        : tr("Delete {n} files from the printer?", { n: paths.length });
    if (!(await confirmDialog(msg))) return;
    try {
      const failed = await api.printerDelete(d.ip, paths);
      if (failed.length) toast("error", `${tr("Could not delete")}: ${failed.map(basename).join(", ")}`);
      setChecked(new Set());
      void load();
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  const print = async (f: FileEntry) => {
    if (!(await confirmDialog(tr("Start printing {file}?", { file: basename(f.path) })))) return;
    try {
      await api.printerCommand(d.ip, "start", f.path);
      toast("success", tr("Print started"));
      useStore.getState().setDeviceTab("overview");
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  const toggle = (p: string) => {
    const n = new Set(checked);
    if (n.has(p)) n.delete(p);
    else n.add(p);
    setChecked(n);
  };

  const shown = useMemo(
    () => (list ?? []).filter((f) => basename(f.path).toLowerCase().includes(query.toLowerCase())),
    [list, query],
  );
  const crumbs = dir.split("/").filter(Boolean);
  const files = shown.filter((f) => !f.is_dir);
  const allChecked = files.length > 0 && files.every((f) => checked.has(f.path));
  const free = d.status?.status.free_storage;
  const progressOf = (f: FileEntry) => downloads[`${d.ip}|${f.path}`];

  const actions = (f: FileEntry) => (
    <>
      <Button small icon="play" disabled={busy} title={busy ? tr("Printer is busy") : tr("Print")} onClick={() => void print(f)} />
      {canDownload && <Button small icon="download" title={tr("Download")} onClick={() => void download(f)} />}
      <Button small icon="trash" variant="ghost" title={tr("Delete")} onClick={() => void remove([f.path])} />
    </>
  );

  return (
    <div className="file-manager">
      <div className="fm-toolbar">
        {!legacy && (
          <div className="segmented">
            {(["/local", "/usb"] as const).map((p) => (
              <button
                key={p}
                className={root === p ? "on" : ""}
                onClick={() => {
                  setRoot(p);
                  setDir(p);
                }}
              >
                {tr(p === "/local" ? "Internal storage" : "USB drive")}
              </button>
            ))}
          </div>
        )}
        <nav className="crumbs" aria-label={tr("Folder")}>
          {crumbs.map((c, i) => (
            <button key={i} onClick={() => setDir("/" + crumbs.slice(0, i + 1).join("/"))} disabled={i === crumbs.length - 1}>
              {i === 0 ? tr(c === "usb" ? "USB drive" : "Internal storage") : c}
            </button>
          ))}
        </nav>
        <span className="spacer" />
        <input
          className="fm-search"
          placeholder={tr("Search files…")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label={tr("Search files")}
        />
        <Button
          small
          icon={view === "grid" ? "slice" : "layout"}
          variant="ghost"
          title={tr(view === "grid" ? "List view" : "Grid view")}
          onClick={() => setView(view === "grid" ? "list" : "grid")}
        />
        <Button small icon="refresh" variant="ghost" title={tr("Refresh")} onClick={() => void load()} />
        <Button small icon="upload" variant="primary" onClick={() => void pickUpload()}>
          {tr("Upload")}
        </Button>
      </div>

      {checked.size > 0 && (
        <div className="fm-bulk">
          <span>
            {checked.size} {tr("selected")}
          </span>
          <Button small icon="trash" variant="danger" onClick={() => void remove([...checked])}>
            {tr("Delete")}
          </Button>
          <Button small variant="ghost" onClick={() => setChecked(new Set())}>
            {tr("Clear selection")}
          </Button>
        </div>
      )}

      {queue.length > 0 && (
        <div className="fm-queue">
          {queue.map((q) => (
            <div key={q.path} className="fm-queue-item">
              <Icon name="upload" size={14} />
              <span className="name">{q.name}</span>
              {q.state === "uploading" && d.upload && !d.upload.done ? (
                <Progress
                  value={d.upload.sent / Math.max(1, d.upload.total)}
                  label={`${Math.round((d.upload.sent / Math.max(1, d.upload.total)) * 100)} %`}
                />
              ) : (
                <span className={`small ${q.state === "error" ? "error-text" : "muted"}`}>
                  {q.state === "error"
                    ? q.error
                    : tr(q.state === "done" ? "Uploaded" : q.state === "queued" ? "Waiting" : "Uploading…")}
                </span>
              )}
              {q.state === "uploading" && (
                <Button small variant="ghost" icon="close" title={tr("Cancel upload")} onClick={() => void api.cancelUpload(d.ip)} />
              )}
            </div>
          ))}
          {!queue.some((q) => q.state === "queued" || q.state === "uploading") && (
            <Button small variant="ghost" onClick={() => setQueue([])}>
              {tr("Clear list")}
            </Button>
          )}
        </div>
      )}

      {error && <p className="muted pad">{error}</p>}
      {list === null ? (
        <p className="muted pad">{tr("Loading…")}</p>
      ) : shown.length === 0 && !error ? (
        <div className="fm-empty">
          <Icon name="upload" size={34} />
          <p>{query ? tr("No files match your search.") : tr("No printable files. Drop sliced files here to upload them.")}</p>
        </div>
      ) : view === "grid" ? (
        <div className="fm-grid">
          {shown.map((f) =>
            f.is_dir ? (
              <button key={f.path} className="fm-tile folder" onClick={() => setDir(f.path)}>
                <Icon name="open" size={40} />
                <span className="name">{basename(f.path)}</span>
              </button>
            ) : (
              <div key={f.path} className={`fm-tile ${checked.has(f.path) ? "checked" : ""}`}>
                <label className="fm-check">
                  <input type="checkbox" checked={checked.has(f.path)} onChange={() => toggle(f.path)} aria-label={basename(f.path)} />
                </label>
                <RemoteThumb ip={d.ip} path={f.path} size={f.size} enabled={canDownload} />
                <span className="name" title={basename(f.path)}>
                  {basename(f.path)}
                </span>
                {progressOf(f) && <Progress value={progressOf(f).done / Math.max(1, progressOf(f).total)} />}
                <div className="fm-actions">{actions(f)}</div>
              </div>
            ),
          )}
        </div>
      ) : (
        <table className="fm-table">
          <thead>
            <tr>
              <th>
                <input
                  type="checkbox"
                  checked={allChecked}
                  onChange={() => setChecked(allChecked ? new Set() : new Set(files.map((f) => f.path)))}
                  aria-label={tr("Select all")}
                />
              </th>
              <th>{tr("Name")}</th>
              <th>{tr("Size")}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {shown.map((f) => (
              <tr key={f.path} className={checked.has(f.path) ? "checked" : ""}>
                <td>
                  {!f.is_dir && (
                    <input type="checkbox" checked={checked.has(f.path)} onChange={() => toggle(f.path)} aria-label={basename(f.path)} />
                  )}
                </td>
                <td className="name">
                  <Icon name={f.is_dir ? "open" : "file"} size={14} />{" "}
                  {f.is_dir ? (
                    <button className="link" onClick={() => setDir(f.path)}>
                      {basename(f.path)}
                    </button>
                  ) : (
                    basename(f.path)
                  )}
                </td>
                <td className="muted">{f.size != null ? fmtBytes(f.size) : ""}</td>
                <td className="actions">{!f.is_dir && actions(f)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <footer className="fm-footer muted small">
        <span>
          {list && tr(files.length === 1 ? "{n} file" : "{n} files", { n: files.length })}
          {free != null && ` · ${fmtBytes(free)} ${tr("free")}`}
        </span>
        <span className="spacer" />
        <span>{tr("Tip: drag sliced files from your computer onto this window to upload them.")}</span>
      </footer>
    </div>
  );
}

const thumbCache = new Map<string, string | null>();

function RemoteThumb({ ip, path, size, enabled }: { ip: string; path: string; size?: number | null; enabled: boolean }) {
  // the size is part of the key so a re-uploaded file with the same name gets a fresh thumbnail
  const key = `${ip}|${path}|${size ?? ""}`;
  const [url, setUrl] = useState<string | null | undefined>(thumbCache.get(key));
  const ref = useRef<HTMLDivElement>(null);
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  useEffect(() => {
    if (!enabled || url !== undefined || !["goo", "ctb"].includes(ext)) return;
    const el = ref.current;
    if (!el) return;
    // load lazily when the tile becomes visible
    const io = new IntersectionObserver((entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      io.disconnect();
      api
        .printerThumbnail(ip, path)
        .then((buf) => {
          const u = URL.createObjectURL(new Blob([buf], { type: "image/png" }));
          thumbCache.set(key, u);
          setUrl(u);
        })
        .catch(() => {
          thumbCache.set(key, null);
          setUrl(null);
        });
    });
    io.observe(el);
    return () => io.disconnect();
  }, [enabled, url, ext, ip, path, key]);
  return (
    <div className="fm-thumb" ref={ref}>
      {url ? <img src={url} alt="" /> : <span className="fm-ext">.{ext}</span>}
    </div>
  );
}

interface Job {
  TaskName?: string;
  BeginTime?: number;
  EndTime?: number;
  TaskStatus?: number;
  AlreadyPrintLayer?: number;
  CurrentLayerTalVolume?: number;
  SliceInformation?: { total_layer_numbers?: number };
}

const JOB_STATUS: Record<number, string> = { 0: "Other", 1: "Completed", 2: "Error", 3: "Stopped" };

function HistoryTable({ d }: { d: DeviceEntry }) {
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api
      .printerHistory(d.ip)
      .then((v) => setJobs(Array.isArray(v) ? (v as Job[]) : []))
      .catch((e) => {
        setError(errorText(e));
        setJobs([]);
      });
  }, [d.ip]);
  const fmtTime = (t?: number) => (t ? new Date(t * 1000).toLocaleString() : "–");
  if (jobs === null) return <p className="muted pad">{tr("Loading…")}</p>;
  if (!jobs.length) return <p className="muted pad">{error ?? tr("No print jobs recorded.")}</p>;
  return (
    <table className="history">
      <thead>
        <tr>
          <th>{tr("File")}</th>
          <th>{tr("Started")}</th>
          <th>{tr("Duration")}</th>
          <th>{tr("Layers")}</th>
          <th>{tr("Resin")}</th>
          <th>{tr("Result")}</th>
        </tr>
      </thead>
      <tbody>
        {jobs.map((j, i) => (
          <tr key={i}>
            <td title={j.TaskName}>{basename(j.TaskName ?? "")}</td>
            <td>{fmtTime(j.BeginTime)}</td>
            <td>{j.BeginTime && j.EndTime && j.EndTime > j.BeginTime ? fmtDuration(j.EndTime - j.BeginTime) : "–"}</td>
            <td>
              {j.AlreadyPrintLayer ?? "–"}
              {j.SliceInformation?.total_layer_numbers ? ` / ${j.SliceInformation.total_layer_numbers}` : ""}
            </td>
            <td>{j.CurrentLayerTalVolume != null ? `${j.CurrentLayerTalVolume.toFixed(1)} ml` : "–"}</td>
            <td>
              <span className={`state-pill ${j.TaskStatus === 1 ? "completed" : j.TaskStatus === 2 ? "error" : "stopped"}`}>
                {tr(JOB_STATUS[j.TaskStatus ?? 0] ?? "Other")}
              </span>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
