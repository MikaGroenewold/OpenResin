import { useEffect, useState } from "react";
import { api, errorText, type LayerParams, type Motion, type PrintSettings, type PrinterProfile } from "../api";
import { useStore } from "../store";
import { Button, Modal, NumberField, Toggle, confirmDialog } from "../ui";
import { t as tr } from "../i18n";

type Tab = "exposure" | "motion" | "resin" | "printer";

/** Print/resin settings and the printer's machine definition. */
export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const config = useStore((s) => s.config)!;
  const setConfig = useStore((s) => s.setConfig);
  const setSlice = useStore((s) => s.setSlice);
  const toast = useStore((s) => s.toast);
  const [tab, setTab] = useState<Tab>("exposure");
  const [s, setS] = useState<PrintSettings>(structuredClone(config.settings));
  const [p, setP] = useState<PrinterProfile>(structuredClone(config.printer));

  const save = async () => {
    try {
      await api.setSettings(s);
      let cfg = await api.getConfig();
      const machineChanged =
        p.resolution_x !== config.printer.resolution_x ||
        p.resolution_y !== config.printer.resolution_y ||
        p.size_x !== config.printer.size_x ||
        p.size_y !== config.printer.size_y ||
        p.size_z !== config.printer.size_z ||
        p.mirror !== config.printer.mirror;
      if (machineChanged) {
        const custom: PrinterProfile = {
          ...p,
          id: p.id.startsWith("custom-") ? p.id : `custom-${p.id}`,
          name: p.name.endsWith("(custom)") ? p.name : `${p.name} (custom)`,
        };
        cfg = await api.saveCustomPrinter(custom);
        await api.setSettings(s);
        cfg = await api.getConfig();
      }
      setConfig(cfg);
      setSlice(null);
      onClose();
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  const layer = (k: "bottom" | "normal", patch: Partial<LayerParams>) => setS({ ...s, [k]: { ...s[k], ...patch } });
  const motion = (k: "bottom" | "normal", m: "lift" | "retract", patch: Partial<Motion>) =>
    layer(k, { [m]: { ...s[k][m], ...patch } } as Partial<LayerParams>);

  return (
    <Modal
      title={tr("Print settings")}
      onClose={onClose}
      wide
      footer={
        <>
          <Button onClick={onClose}>{tr("Cancel")}</Button>
          <Button variant="primary" onClick={() => void save()}>
            {tr("Save")}
          </Button>
        </>
      }
    >
      <PresetBar s={s} onLoad={setS} />
      <div className="tabs" role="tablist">
        {(
          [
            ["exposure", "Exposure"],
            ["motion", "Lift & retract"],
            ["resin", "Resin"],
            ["printer", "Machine"],
          ] as [Tab, string][]
        ).map(([id, label]) => (
          <button key={id} role="tab" aria-selected={tab === id} className={tab === id ? "on" : ""} onClick={() => setTab(id)}>
            {tr(label)}
          </button>
        ))}
      </div>

      {tab === "exposure" && (
        <div className="settings-grid">
          <NumberField label={tr("Layer height")} unit="mm" step={0.01} min={0.01} max={0.3} value={s.layer_height} onChange={(v) => setS({ ...s, layer_height: v })} />
          <NumberField label={tr("Bottom layers")} step={1} min={0} max={100} value={s.bottom_layers} onChange={(v) => setS({ ...s, bottom_layers: Math.round(v) })} />
          <NumberField label={tr("Transition layers")} step={1} min={0} max={100} value={s.transition_layers} onChange={(v) => setS({ ...s, transition_layers: Math.round(v) })} />
          <NumberField label={tr("Exposure time")} unit="s" step={0.1} min={0.1} value={s.normal.exposure} onChange={(v) => layer("normal", { exposure: v })} />
          <NumberField label={tr("Bottom exposure")} unit="s" step={1} min={0.1} value={s.bottom.exposure} onChange={(v) => layer("bottom", { exposure: v })} />
          <label className="field">
            <span className="field-label">{tr("Anti-aliasing")}</span>
            <select value={s.anti_aliasing} onChange={(e) => setS({ ...s, anti_aliasing: +e.target.value })}>
              {[1, 2, 4, 8, 16].map((a) => (
                <option key={a} value={a}>
                  {a === 1 ? tr("Off") : `${a}×`}
                </option>
              ))}
            </select>
          </label>
          <NumberField label={tr("UV power (PWM)")} step={5} min={1} max={255} value={s.normal.light_pwm} onChange={(v) => layer("normal", { light_pwm: Math.round(v) })} />
          <NumberField label={tr("Bottom UV power")} step={5} min={1} max={255} value={s.bottom.light_pwm} onChange={(v) => layer("bottom", { light_pwm: Math.round(v) })} />
          <div className="span-all">
            <Toggle label={tr("Use rest times (wait mode) instead of light-off delay")} checked={s.wait_mode} onChange={(v) => setS({ ...s, wait_mode: v })} />
          </div>
          {s.wait_mode ? (
            <>
              <NumberField label={tr("Rest before lift")} unit="s" step={0.5} min={0} value={s.normal.wait_before_lift} onChange={(v) => layer("normal", { wait_before_lift: v })} />
              <NumberField label={tr("Rest after lift")} unit="s" step={0.5} min={0} value={s.normal.wait_after_lift} onChange={(v) => layer("normal", { wait_after_lift: v })} />
              <NumberField label={tr("Rest after retract")} unit="s" step={0.5} min={0} value={s.normal.wait_after_retract} onChange={(v) => layer("normal", { wait_after_retract: v })} />
              <NumberField label={tr("Bottom rest before lift")} unit="s" step={0.5} min={0} value={s.bottom.wait_before_lift} onChange={(v) => layer("bottom", { wait_before_lift: v })} />
              <NumberField label={tr("Bottom rest after lift")} unit="s" step={0.5} min={0} value={s.bottom.wait_after_lift} onChange={(v) => layer("bottom", { wait_after_lift: v })} />
              <NumberField label={tr("Bottom rest after retract")} unit="s" step={0.5} min={0} value={s.bottom.wait_after_retract} onChange={(v) => layer("bottom", { wait_after_retract: v })} />
            </>
          ) : (
            <>
              <NumberField label={tr("Light-off delay")} unit="s" step={0.5} min={0} value={s.normal.light_off_delay} onChange={(v) => layer("normal", { light_off_delay: v })} />
              <NumberField label={tr("Bottom light-off delay")} unit="s" step={0.5} min={0} value={s.bottom.light_off_delay} onChange={(v) => layer("bottom", { light_off_delay: v })} />
            </>
          )}
        </div>
      )}

      {tab === "motion" && (
        <div className="motion-grid">
          {(["bottom", "normal"] as const).map((k) => (
            <fieldset key={k}>
              <legend>{tr(k === "bottom" ? "Bottom layers" : "Normal layers")}</legend>
              <div className="settings-grid">
                <NumberField label={tr("Lift distance")} unit="mm" step={0.5} min={0} value={s[k].lift.distance} onChange={(v) => motion(k, "lift", { distance: v })} />
                <NumberField label={tr("Lift speed")} unit="mm/min" step={5} min={1} value={s[k].lift.speed} onChange={(v) => motion(k, "lift", { speed: v })} />
                <NumberField label={tr("Lift distance 2")} unit="mm" step={0.5} min={0} value={s[k].lift.distance2} onChange={(v) => motion(k, "lift", { distance2: v })} />
                <NumberField label={tr("Lift speed 2")} unit="mm/min" step={5} min={0} value={s[k].lift.speed2} onChange={(v) => motion(k, "lift", { speed2: v })} />
                <NumberField label={tr("Retract distance")} unit="mm" step={0.5} min={0} value={s[k].retract.distance} onChange={(v) => motion(k, "retract", { distance: v })} />
                <NumberField label={tr("Retract speed")} unit="mm/min" step={5} min={1} value={s[k].retract.speed} onChange={(v) => motion(k, "retract", { speed: v })} />
                <NumberField label={tr("Retract distance 2")} unit="mm" step={0.5} min={0} value={s[k].retract.distance2} onChange={(v) => motion(k, "retract", { distance2: v })} />
                <NumberField label={tr("Retract speed 2")} unit="mm/min" step={5} min={0} value={s[k].retract.speed2} onChange={(v) => motion(k, "retract", { speed2: v })} />
              </div>
            </fieldset>
          ))}
          <p className="muted small span-all">
            {tr(
              "Two-stage motion: the first distance runs at the first speed, then the second distance at the second speed. Leave the second stage at 0 for single-speed moves.",
            )}
          </p>
        </div>
      )}

      {tab === "resin" && (
        <div className="settings-grid">
          <label className="field span-2">
            <span className="field-label">{tr("Profile name")}</span>
            <input value={s.name} onChange={(e) => setS({ ...s, name: e.target.value })} />
          </label>
          <NumberField label={tr("Density")} unit="g/ml" step={0.01} min={0.5} max={3} value={s.resin_density} onChange={(v) => setS({ ...s, resin_density: v })} />
          <NumberField label={tr("Price per litre")} step={1} min={0} value={s.resin_price_per_liter} onChange={(v) => setS({ ...s, resin_price_per_liter: v })} />
          <label className="field">
            <span className="field-label">{tr("Currency")}</span>
            <input value={s.currency} maxLength={6} onChange={(e) => setS({ ...s, currency: e.target.value })} />
          </label>
        </div>
      )}

      {tab === "printer" && (
        <div className="settings-grid">
          <p className="muted small span-all">
            {tr("Changing the machine geometry stores a custom copy of “{printer}”.", { printer: config.printer.name })}
          </p>
          <NumberField label={tr("Resolution X")} unit="px" step={1} min={1} value={p.resolution_x} onChange={(v) => setP({ ...p, resolution_x: Math.round(v) })} />
          <NumberField label={tr("Resolution Y")} unit="px" step={1} min={1} value={p.resolution_y} onChange={(v) => setP({ ...p, resolution_y: Math.round(v) })} />
          <NumberField label={tr("Display width")} unit="mm" step={0.1} min={1} value={p.size_x} onChange={(v) => setP({ ...p, size_x: v })} />
          <NumberField label={tr("Display depth")} unit="mm" step={0.1} min={1} value={p.size_y} onChange={(v) => setP({ ...p, size_y: v })} />
          <NumberField label={tr("Max height")} unit="mm" step={1} min={1} value={p.size_z} onChange={(v) => setP({ ...p, size_z: v })} />
          <label className="field">
            <span className="field-label">{tr("Image orientation")}</span>
            <select value={p.mirror} onChange={(e) => setP({ ...p, mirror: e.target.value as PrinterProfile["mirror"] })}>
              <option value="lcd">{tr("LCD (mirrored, standard)")}</option>
              <option value="none">{tr("Not mirrored (DLP)")}</option>
              <option value="x">{tr("Mirror X")}</option>
              <option value="y">{tr("Mirror Y")}</option>
            </select>
          </label>
          <p className="muted small span-all">
            {tr("Pixel size: {x} × {y} µm", {
              x: ((p.size_x / p.resolution_x) * 1000).toFixed(1),
              y: ((p.size_y / p.resolution_y) * 1000).toFixed(1),
            })}
          </p>
        </div>
      )}
    </Modal>
  );
}

function PresetBar({ s, onLoad }: { s: PrintSettings; onLoad: (s: PrintSettings) => void }) {
  const toast = useStore((st) => st.toast);
  const [list, setList] = useState<{ settings: PrintSettings; builtin: boolean }[]>([]);
  const [naming, setNaming] = useState<string | null>(null);
  const reload = () => void api.listPresets().then(setList).catch(() => {});
  useEffect(reload, []);
  const current = list.find((p) => p.settings.name === s.name);
  return (
    <div className="preset-bar">
      <label className="field grow">
        <span className="field-label">{tr("Resin profile")}</span>
        <select
          value={current ? s.name : ""}
          onChange={(e) => {
            const p = list.find((x) => x.settings.name === e.target.value);
            if (p) onLoad(structuredClone(p.settings));
          }}
        >
          {!current && <option value="">{tr("{name} (modified)", { name: s.name })}</option>}
          {list.map((p) => (
            <option key={p.settings.name} value={p.settings.name}>
              {p.settings.name}
              {p.builtin ? "" : " ★"}
            </option>
          ))}
        </select>
      </label>
      {naming == null ? (
        <>
          <Button small icon="save" onClick={() => setNaming(current?.builtin ? `${s.name} (copy)` : s.name)}>
            {tr("Save as preset")}
          </Button>
          {current && !current.builtin && (
            <Button
              small
              icon="trash"
              variant="ghost"
              title={tr("Delete preset")}
              onClick={async () => {
                if (!(await confirmDialog(tr("Delete the preset “{name}”?", { name: s.name })))) return;
                await api.deletePreset(s.name);
                reload();
              }}
            />
          )}
        </>
      ) : (
        <>
          <input
            className="preset-name"
            autoFocus
            value={naming}
            onChange={(e) => setNaming(e.target.value)}
            aria-label={tr("Preset name")}
            onKeyDown={(e) => {
              if (e.key !== "Escape") return;
              // cancel only the naming; the dialog's Escape handler would discard all edits
              e.stopPropagation();
              setNaming(null);
            }}
          />
          <Button
            small
            variant="primary"
            disabled={!naming.trim()}
            onClick={async () => {
              try {
                const next = { ...s, name: naming.trim() };
                await api.savePreset(next);
                onLoad(next);
                setNaming(null);
                reload();
                toast("success", tr("Saved preset “{name}”", { name: next.name }));
              } catch (e) {
                toast("error", errorText(e));
              }
            }}
          >
            {tr("Save")}
          </Button>
          <Button small variant="ghost" onClick={() => setNaming(null)}>
            {tr("Cancel")}
          </Button>
        </>
      )}
    </div>
  );
}
