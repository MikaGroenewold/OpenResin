import { useMemo, useState } from "react";
import { api, errorText } from "../api";
import { useStore } from "../store";
import { Icon, Modal } from "../ui";
import { t as tr } from "../i18n";

/** Searchable list of all known printers. */
export function PrinterPicker({ onClose }: { onClose: () => void }) {
  const printers = useStore((s) => s.printers);
  const current = useStore((s) => s.config?.printer.id);
  const setConfig = useStore((s) => s.setConfig);
  const setSlice = useStore((s) => s.setSlice);
  const toast = useStore((s) => s.toast);
  const [q, setQ] = useState("");
  const groups = useMemo(() => {
    const ql = q.toLowerCase();
    const map = new Map<string, typeof printers>();
    for (const p of printers) {
      if (ql && !`${p.vendor} ${p.name}`.toLowerCase().includes(ql)) continue;
      const v = p.vendor || tr("Other");
      if (!map.has(v)) map.set(v, []);
      map.get(v)!.push(p);
    }
    return [...map.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  }, [printers, q]);

  const pick = async (id: string) => {
    try {
      setConfig(await api.setPrinter(id));
      setSlice(null);
      onClose();
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  return (
    <Modal title={tr("Choose printer")} onClose={onClose} wide>
      <input
        className="search"
        autoFocus
        placeholder={tr("Search printers…")}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        aria-label={tr("Search printers")}
      />
      <div className="printer-list">
        {groups.map(([vendor, list]) => (
          <div key={vendor} className="printer-group">
            <h4>{vendor}</h4>
            {list.map((p) => (
              <button key={p.id} className={`printer-row ${p.id === current ? "on" : ""}`} onClick={() => void pick(p.id)}>
                <span className="name">{p.name}</span>
                <span className="muted small">
                  {p.resolution_x}×{p.resolution_y} · {p.size_x.toFixed(1)}×{p.size_y.toFixed(1)}×{p.size_z.toFixed(0)} mm
                </span>
                <span className="formats small">{p.formats.map((f) => `.${f}`).join(" ")}</span>
                {p.network === "sdcp" && (
                  <span className="badge net" title={tr("Network printing and camera supported")}>
                    <Icon name="wifi" size={12} /> {tr("LAN")}
                  </span>
                )}
              </button>
            ))}
          </div>
        ))}
        {groups.length === 0 && <p className="muted">{tr("No printer matches “{query}”.", { query: q })}</p>}
      </div>
    </Modal>
  );
}
