import { useStore, type Panel, type Ribbon as RibbonId, type Tool } from "../store";
import { autoSupports, exportStl, openModelsDialog, saveProject, saveSliced, sliceNow } from "../actions";
import { combine } from "../editActions";
import { splitSelected } from "./GeometryPanels";
import { Icon } from "../ui";
import { t as tr } from "../i18n";

interface Item {
  id: string;
  label: string;
  icon: string;
  tool?: Tool;
  panel?: Panel;
  action?: () => void;
  active?: () => boolean;
  needsSelection?: boolean;
  needsObjects?: boolean;
}

const select: Item = { id: "select", label: "Select", icon: "select", tool: "select" };
const measure: Item = { id: "measure", label: "Measure", icon: "measure", tool: "measure", needsObjects: true };

export const RIBBON_TABS: { id: RibbonId; label: string; icon: string }[] = [
  { id: "repair", label: "Repair", icon: "repair" },
  { id: "prepare", label: "Prepare", icon: "cube" },
  { id: "support", label: "Support", icon: "supports" },
  { id: "export", label: "Export", icon: "upload" },
];

/** Extra items registered by optional tools (repair, hollow, cut, …). */
export const extraItems: Partial<Record<RibbonId, Item[]>> = {};

function items(tab: RibbonId): Item[] {
  const s = useStore.getState();
  const base: Record<RibbonId, Item[]> = {
    repair: [
      select,
      measure,
      { id: "detect", label: "Error Detection", icon: "detect", panel: "repair", needsObjects: true },
      { id: "repair", label: "Model Repair", icon: "repair", panel: "repair", needsObjects: true },
      { id: "split-r", label: "Split Model", icon: "split", action: () => void splitSelected(), needsSelection: true },
    ],
    prepare: [
      select,
      { id: "move", label: "Move", icon: "move", tool: "move", needsObjects: true },
      { id: "rotate", label: "Rotate", icon: "rotate", tool: "rotate", needsObjects: true },
      { id: "scale", label: "Scale", icon: "scale", tool: "scale", needsObjects: true },
      measure,
      { id: "layflat", label: "Lay Flat", icon: "flat", tool: "layflat", needsObjects: true },
      { id: "orient", label: "Auto Orient", icon: "orient", panel: "orient", needsObjects: true },
      { id: "platform", label: "Add Platform", icon: "platform", panel: "platform", needsObjects: true },
      { id: "layout", label: "Auto Layout", icon: "layout", panel: "layout", needsObjects: true },
      { id: "hollow", label: "Hollow", icon: "hollow", panel: "hollow", needsObjects: true },
      {
        id: "drill",
        label: "Drill Holes",
        icon: "drill",
        tool: "drill",
        panel: "hollow",
        needsObjects: true,
      },
      { id: "copy", label: "Copy / Fill", icon: "copy", panel: "copy", needsSelection: true },
      { id: "mirror", label: "Mirror", icon: "mirror", panel: "mirror" as Panel, needsSelection: true },
      { id: "split", label: "Split Model", icon: "split", action: () => void splitSelected(), needsSelection: true },
      { id: "cut", label: "Cut", icon: "cut", panel: "cut", needsSelection: true },
      { id: "boolean", label: "Boolean Operation", icon: "boolean", panel: "boolean", needsObjects: true },
      { id: "label", label: "Add Label", icon: "label", tool: "label", panel: "label", needsObjects: true },
      { id: "combine", label: "Combine Models", icon: "combine", action: () => void combine(), needsSelection: true },
      { id: "align", label: "Align", icon: "align", panel: "align", needsObjects: true },
    ],
    support: [
      select,
      measure,
      { id: "edit", label: "Remove Support", icon: "edit_support", tool: "support_remove", needsObjects: true },
      { id: "manual", label: "Manual Support", icon: "manual_support", tool: "support_add", needsObjects: true },
      { id: "auto", label: "Auto Support", icon: "auto_support", panel: "support", needsObjects: true },
      {
        id: "magic",
        label: "Magic Support",
        icon: "magic",
        needsObjects: true,
        action: () => {
          const st = useStore.getState();
          const ids = st.selection.length ? st.selection : st.objects.map((o) => o.id);
          void autoSupports(ids, { magic: true });
        },
      },
      { id: "raft", label: "Raft", icon: "raft", panel: "raft" },
      {
        id: "overhang",
        label: "Overhang Detection",
        icon: "overhang",
        action: () => s.setOverhang(!useStore.getState().overhang),
        active: () => useStore.getState().overhang,
      },
    ],
    export: [
      select,
      measure,
      { id: "slice", label: "Slice", icon: "slice", action: () => void sliceNow(null), needsObjects: true },
      { id: "settings", label: "Slice Settings", icon: "settings", action: () => window.dispatchEvent(new Event("open-settings")) },
      { id: "export-model", label: "Export Model", icon: "export_model", action: () => void exportStl(), needsObjects: true },
      { id: "save", label: "Save Project", icon: "save", action: () => void saveProject() },
      { id: "save-sliced", label: "Save Sliced File", icon: "download", action: () => void saveSliced() },
    ],
  };
  const list = [...base[tab]];
  for (const extra of extraItems[tab] ?? []) {
    // keep "select"/"measure" first, then registered tools before the built-ins of the tab
    list.splice(2, 0, extra);
  }
  return list;
}

export function Ribbon() {
  const tab = useStore((s) => s.ribbon);
  const tool = useStore((s) => s.tool);
  const panel = useStore((s) => s.panel);
  const selection = useStore((s) => s.selection);
  const objects = useStore((s) => s.objects);
  useStore((s) => s.overhang);
  useStore((s) => s.slice);
  const setTool = useStore((s) => s.setTool);
  const setPanel = useStore((s) => s.setPanel);
  const list = items(tab);
  const tabLabel = RIBBON_TABS.find((r) => r.id === tab)?.label ?? tab;
  return (
    <div className="ribbon" role="toolbar" aria-label={tr("{tab} tools", { tab: tr(tabLabel) })}>
      {list.map((it) => {
        const disabled =
          (it.needsSelection && selection.length === 0) ||
          (it.needsObjects && objects.length === 0) ||
          (it.id === "save-sliced" && !useStore.getState().slice);
        const active = it.active ? it.active() : it.tool ? tool === it.tool : it.panel ? panel === it.panel : false;
        return (
          <button
            key={it.id}
            className={`ribbon-btn ${active ? "on" : ""}`}
            disabled={disabled}
            title={tr(it.label)}
            onClick={() => {
              if (it.tool) {
                setTool(it.tool);
                if (it.tool === "support_add" || it.tool === "support_remove") useStore.getState().setPanel(null);
              }
              if (it.panel) {
                // tools that come with a panel always show it (no toggle)
                if (it.tool) useStore.setState({ panel: it.panel });
                else setPanel(it.panel);
              }
              it.action?.();
            }}
          >
            <Icon name={it.icon} size={22} />
            <span>{tr(it.label)}</span>
          </button>
        );
      })}
      <span className="spacer" />
      {objects.length === 0 && (
        <button className="ribbon-btn" onClick={() => void openModelsDialog()}>
          <Icon name="open" size={22} />
          <span>{tr("Open Models")}</span>
        </button>
      )}
    </div>
  );
}
