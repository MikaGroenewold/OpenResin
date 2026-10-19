import { useEffect, useState } from "react";
import { api, errorText } from "../api";
import { connectDevice, discover } from "../actions";
import { fmtBytes } from "../format";
import { useStore } from "../store";
import { Button, Modal, Progress, Toggle } from "../ui";
import { t as tr } from "../i18n";
import { stateLabel } from "./DevicesView";

/** Uploads the current slice result to a network printer. */
export function SendDialog({ onClose, initial = null }: { onClose: () => void; initial?: string | null }) {
  const devices = useStore((s) => s.devices);
  const slice = useStore((s) => s.slice);
  const toast = useStore((s) => s.toast);
  const [target, setTarget] = useState<string | null>(initial);
  const [start, setStart] = useState(false);
  const [name, setName] = useState(slice?.suggested_name ?? "job");
  const [sending, setSending] = useState(false);
  const list = Object.values(devices);
  const dev = target ? devices[target] : undefined;

  useEffect(() => {
    if (!list.length) void discover();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (target) return;
    const connected = list.filter((d) => d.connected);
    if (connected.length >= 1) setTarget(connected[0].ip);
    else if (list.length === 1) setTarget(list[0].ip);
  }, [list, target]);

  const busy = dev?.status?.status.state ? ["printing", "paused", "pausing", "file_transfer"].includes(dev.status.status.state) : false;

  const send = async () => {
    if (!target) return;
    setSending(true);
    try {
      if (!devices[target]?.connected && !(await connectDevice(target))) {
        throw new Error(useStore.getState().devices[target]?.error ?? tr("Could not connect."));
      }
      const remote = await api.uploadToPrinter(target, start, name);
      toast("success", tr(start ? "Uploaded {file} and started printing" : "Uploaded {file}", { file: remote }));
      onClose();
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setSending(false);
    }
  };

  const up = dev?.upload;
  return (
    <Modal
      title={tr("Send to printer")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{tr("Close")}</Button>
          {sending && target ? (
            <Button variant="danger" onClick={() => void api.cancelUpload(target)}>
              {tr("Cancel upload")}
            </Button>
          ) : (
            <Button variant="primary" icon="upload" disabled={!target || sending} onClick={() => void send()}>
              {tr(start ? "Upload & print" : "Upload")}
            </Button>
          )}
        </>
      }
    >
      <div className="device-pick">
        {list.length === 0 && <p className="muted">{tr("No printers known yet. Searching the local network…")}</p>}
        {list.map((d) => (
          <label key={d.ip} className={`device-option ${target === d.ip ? "on" : ""}`}>
            <input type="radio" name="dev" checked={target === d.ip} onChange={() => setTarget(d.ip)} />
            <span className="name">{d.name}</span>
            <span className="muted small">
              {d.model ?? d.discovered?.model ?? ""} · {d.ip}
            </span>
            {d.status && <span className="badge">{stateLabel(d.status.status.state)}</span>}
          </label>
        ))}
        <Button small icon="refresh" onClick={() => void discover()}>
          {tr("Search again")}
        </Button>
      </div>
      <label className="field">
        <span className="field-label">{tr("File name on the printer")}</span>
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <Toggle label={tr("Start printing after upload")} checked={start} onChange={setStart} disabled={busy} />
      {busy && <p className="warn-text small">{tr("The printer is busy; the file will only be uploaded.")}</p>}
      <p className="muted small">
        {slice
          ? `${fmtBytes(slice.file_size)} · ${tr(slice.layer_count === 1 ? "{n} layer" : "{n} layers", { n: slice.layer_count })} · .${slice.extension}`
          : ""}
      </p>
      {up && !up.done && <Progress value={up.sent / Math.max(1, up.total)} label={`${fmtBytes(up.sent)} / ${fmtBytes(up.total)}`} />}
    </Modal>
  );
}
