import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SliceSummary } from "../api";
import { useStore } from "../store";
import { PrinterPicker } from "../views/PrinterPicker";
import { SettingsDialog } from "../views/SettingsDialog";
import { defaultSettings } from "./fakeBackend";
import { backend, renderWithConfig, setNumber, toasts } from "./harness";

const st = () => useStore.getState();
const field = (label: string) => screen.getByLabelText(new RegExp(`^${label.replace(/[()]/g, "\\$&")}`));

async function openSettings() {
  const onClose = vi.fn();
  await renderWithConfig(<SettingsDialog onClose={onClose} />);
  const presetSelect = () => screen.getByLabelText(/^Resin profile/) as HTMLSelectElement;
  return { onClose, presetSelect };
}

describe("SettingsDialog presets", () => {
  it("lists built-in and user presets and loads one into the form", async () => {
    const user = userEvent.setup();
    backend.userPresets = [{ ...defaultSettings(), name: "My resin", layer_height: 0.03 }];
    const { presetSelect } = await openSettings();
    await waitFor(() => expect(within(presetSelect()).getAllByRole("option")).toHaveLength(3));
    expect(within(presetSelect()).getAllByRole("option").map((o) => o.textContent)).toEqual(["Standard resin", "ABS-like resin", "My resin ★"]);
    expect(presetSelect()).toHaveValue("Standard resin");
    await user.selectOptions(presetSelect(), "ABS-like resin");
    expect(field("Exposure time")).toHaveValue("3");
    expect(field("Bottom exposure")).toHaveValue("35");
    await user.selectOptions(presetSelect(), "My resin");
    expect(field("Layer height")).toHaveValue("0.03");
  });

  it("saves the current settings as a new preset", async () => {
    const user = userEvent.setup();
    const { presetSelect } = await openSettings();
    await waitFor(() => expect(within(presetSelect()).getAllByRole("option")).toHaveLength(2));
    setNumber(field("Exposure time"), 2.2);
    await user.click(screen.getByRole("button", { name: "Save as preset" }));
    const name = screen.getByRole("textbox", { name: "Preset name" });
    // a built-in preset is never overwritten: the suggestion is a copy
    expect(name).toHaveValue("Standard resin (copy)");
    const bar = within(name.closest(".preset-bar") as HTMLElement);
    await user.clear(name);
    expect(bar.getByRole("button", { name: "Save" })).toBeDisabled();
    await user.type(name, "Fast grey");
    await user.click(bar.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(backend.callsOf("save_preset")).toHaveLength(1));
    const saved = backend.lastCall("save_preset")!.settings as Record<string, unknown> & { normal: { exposure: number } };
    expect(saved.name).toBe("Fast grey");
    expect(saved.normal.exposure).toBe(2.2);
    expect(toasts()).toContain("success: Saved preset “Fast grey”");
    await waitFor(() => expect(presetSelect()).toHaveValue("Fast grey"));
    expect(within(presetSelect()).getByRole("option", { name: "Fast grey ★" })).toBeInTheDocument();
  });

  it("cancels naming a preset with Escape without closing the dialog (unsaved edits stay)", async () => {
    const user = userEvent.setup();
    const { onClose } = await openSettings();
    setNumber(field("Layer height"), 0.04);
    await user.click(screen.getByRole("button", { name: "Save as preset" }));
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "Preset name" })).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    expect(field("Layer height")).toHaveValue("0.04");
    await user.click(screen.getByRole("button", { name: "Save as preset" }));
    const bar = within(screen.getByRole("textbox", { name: "Preset name" }).closest(".preset-bar") as HTMLElement);
    await user.click(bar.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("textbox", { name: "Preset name" })).toBeNull();
    expect(backend.callsOf("save_preset")).toEqual([]);
    // Escape outside the name field still closes the dialog
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });

  it("deletes a user preset after confirmation", async () => {
    const user = userEvent.setup();
    backend.userPresets = [{ ...defaultSettings(), name: "Old resin" }];
    const { presetSelect } = await openSettings();
    await waitFor(() => expect(within(presetSelect()).getAllByRole("option")).toHaveLength(3));
    expect(screen.queryByRole("button", { name: "Delete preset" })).toBeNull(); // built-in selected
    await user.selectOptions(presetSelect(), "Old resin");
    backend.askAnswers.push(false);
    await user.click(screen.getByRole("button", { name: "Delete preset" }));
    await waitFor(() => expect(backend.asked).toEqual(["Delete the preset “Old resin”?"]));
    expect(backend.callsOf("delete_preset")).toEqual([]);
    backend.askAnswers.push(true);
    await user.click(screen.getByRole("button", { name: "Delete preset" }));
    await waitFor(() => expect(backend.lastCall("delete_preset")).toEqual({ name: "Old resin" }));
    await waitFor(() => expect(within(presetSelect()).getAllByRole("option").map((o) => o.textContent)).toEqual(["Old resin (modified)", "Standard resin", "ABS-like resin"]));
  });

  it("shows preset errors from the backend", async () => {
    const user = userEvent.setup();
    await openSettings();
    backend.fail("save_preset", "exposure must be positive");
    await user.click(screen.getByRole("button", { name: "Save as preset" }));
    const bar = screen.getByRole("textbox", { name: "Preset name" }).closest(".preset-bar") as HTMLElement;
    await user.click(within(bar).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(toasts()).toContain("error: exposure must be positive"));
    expect(screen.getByRole("textbox", { name: "Preset name" })).toBeInTheDocument();
  });
});

describe("SettingsDialog settings", () => {
  it("saves edited settings, refreshes the config and drops the old slice", async () => {
    const user = userEvent.setup();
    const { onClose } = await openSettings();
    st().setSlice({ layer_count: 3 } as SliceSummary);
    setNumber(field("Layer height"), 0.03);
    setNumber(field("Bottom layers"), "5.4");
    await user.selectOptions(screen.getByLabelText(/^Anti-aliasing/), "8");
    await user.click(screen.getByRole("checkbox", { name: "Use rest times (wait mode) instead of light-off delay" }));
    expect(field("Light-off delay")).toBeInTheDocument();
    setNumber(field("Light-off delay"), 1.5);
    await user.click(screen.getByRole("tab", { name: "Lift & retract" }));
    expect(screen.getByRole("tab", { name: "Lift & retract" })).toHaveAttribute("aria-selected", "true");
    const bottom = within(screen.getByRole("group", { name: "Bottom layers" }));
    setNumber(bottom.getByLabelText(/^Lift speed 2/), 120);
    const normal = within(screen.getByRole("group", { name: "Normal layers" }));
    setNumber(normal.getByLabelText(/^Retract distance(?! 2)/), 7);
    await user.click(screen.getByRole("tab", { name: "Resin" }));
    const cur = screen.getByLabelText("Currency");
    await user.clear(cur);
    await user.type(cur, "CHF");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const s = backend.lastCall("set_settings")!.settings as ReturnType<typeof defaultSettings>;
    expect(s).toMatchObject({ layer_height: 0.03, bottom_layers: 5, anti_aliasing: 8, wait_mode: false, currency: "CHF" });
    expect(s.normal.light_off_delay).toBe(1.5);
    expect(s.bottom.lift.speed2).toBe(120);
    expect(s.normal.retract.distance).toBe(7);
    expect(st().config!.settings.layer_height).toBe(0.03);
    expect(st().slice).toBeNull();
    expect(backend.callsOf("save_custom_printer")).toEqual([]);
  });

  it("stores a custom printer when the machine geometry changes", async () => {
    const user = userEvent.setup();
    const { onClose } = await openSettings();
    await user.click(screen.getByRole("tab", { name: "Machine" }));
    expect(screen.getByText("Pixel size: 17.0 × 15.2 µm")).toBeInTheDocument();
    setNumber(field("Max height"), 200);
    await user.selectOptions(screen.getByLabelText(/^Image orientation/), "none");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(backend.lastCall("save_custom_printer")!.printer).toMatchObject({
      id: "custom-elegoo-mars-5-ultra",
      name: "Mars 5 Ultra (custom)",
      size_z: 200,
      mirror: "none",
    });
    expect(backend.callsOf("set_settings")).toHaveLength(2); // settings stay with the new printer
    expect(st().config!.printer.id).toBe("custom-elegoo-mars-5-ultra");
  });

  it("keeps the dialog open when the backend rejects the settings", async () => {
    const user = userEvent.setup();
    const { onClose } = await openSettings();
    backend.fail("set_settings", "bottom exposure must be at least the normal exposure");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(toasts()).toContain("error: bottom exposure must be at least the normal exposure"));
    expect(onClose).not.toHaveBeenCalled();
  });

  it("Cancel closes without saving", async () => {
    const user = userEvent.setup();
    const { onClose } = await openSettings();
    setNumber(field("Layer height"), 0.1);
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(backend.callsOf("set_settings")).toEqual([]);
  });
});

describe("PrinterPicker", () => {
  it("filters printers and switches the printer", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    st().setSlice({ layer_count: 3 } as SliceSummary);
    await renderWithConfig(<PrinterPicker onClose={onClose} />);
    expect(screen.getByRole("heading", { name: "ELEGOO" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Anycubic" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Mars 5 Ultra/ })).toHaveClass("on");
    expect(within(screen.getByRole("button", { name: /Mars 5 Ultra/ })).getByText("LAN")).toBeInTheDocument();
    await user.type(screen.getByRole("textbox", { name: "Search printers" }), "photon");
    expect(screen.queryByRole("button", { name: /Mars 5 Ultra/ })).toBeNull();
    await user.click(screen.getByRole("button", { name: /Photon Mono X/ }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(backend.lastCall("set_printer")).toEqual({ id: "anycubic-photon-mono-x" });
    expect(st().config!.printer.name).toBe("Photon Mono X");
    expect(st().slice).toBeNull();
  });

  it("says when nothing matches", async () => {
    const user = userEvent.setup();
    await renderWithConfig(<PrinterPicker onClose={() => {}} />);
    await user.type(screen.getByRole("textbox", { name: "Search printers" }), "zzz");
    expect(screen.getByText("No printer matches “zzz”.")).toBeInTheDocument();
  });

  it("shows backend errors and stays open", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    await renderWithConfig(<PrinterPicker onClose={onClose} />);
    backend.fail("set_printer", "profile is broken");
    await user.click(screen.getByRole("button", { name: /Photon Mono X/ }));
    await waitFor(() => expect(toasts()).toContain("error: profile is broken"));
    expect(onClose).not.toHaveBeenCalled();
  });
});
