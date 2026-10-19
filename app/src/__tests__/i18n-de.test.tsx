// The UI language is picked when i18n.ts loads: select German before anything imports it.
import { vi } from "vitest";

vi.hoisted(() => localStorage.setItem("openresin.lang", "de"));

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { importPaths } from "../actions";
import { ErrorBoundary } from "../ErrorBoundary";
import { lang, setLang, t } from "../i18n";
import { useStore } from "../store";
import { WelcomeDialog } from "../views/Onboarding";
import { ModelView } from "../views/ModelView";
import { TopBar } from "../views/TopBar";
import { loadConfig, renderWithConfig, seed, toasts } from "./harness";

describe("German UI", () => {
  it("is selected from the stored language", () => {
    expect(lang).toBe("de");
    expect(t("Repair")).toBe("Reparieren");
    expect(t("Not translated yet")).toBe("Not translated yet"); // falls back to English
  });

  it("renders the workflow bar and ribbon in German", async () => {
    seed("cube");
    await renderWithConfig(
      <>
        <TopBar onPickPrinter={() => {}} />
        <ModelView />
      </>,
    );
    const nav = within(screen.getByRole("navigation", { name: "Arbeitsablauf" }));
    for (const name of ["Reparieren", "Vorbereiten", "Stützen", "Export", "Vorschau", "Drucker"]) expect(nav.getByRole("button", { name })).toBeInTheDocument();
    const ribbon = within(screen.getByRole("toolbar"));
    for (const name of ["Auswählen", "Verschieben", "Auto-Ausrichtung", "Plattform hinzufügen", "Kopieren / Füllen", "Beschriftung"])
      expect(ribbon.getByRole("button", { name })).toBeInTheDocument();
    expect(screen.getByTitle("Sprache")).toHaveTextContent("DE");
    expect(screen.getByRole("heading", { name: "Modelle (1)" })).toBeInTheDocument();
    expect(screen.getByRole("contentinfo")).toHaveTextContent("1 Modell");
  });

  it("translates panels opened from the ribbon", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderWithConfig(<ModelView />);
    await user.click(screen.getByRole("button", { name: "Auto-Ausrichtung" }));
    expect(screen.getByRole("radio", { name: /Ausgewogen/ })).toBeChecked();
    expect(screen.getByRole("radio", { name: /Wenigste Stützen/ })).toBeInTheDocument();
    // the Orient action must not look like the Align tool ("Ausrichten") next to it
    expect(screen.getByRole("button", { name: "Optimal ausrichten" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Ausrichten" })).toHaveLength(1);
    useStore.setState({ panel: "align" });
    expect(await screen.findByRole("button", { name: "Hinten" })).toBeInTheDocument();
  });

  it("shows action feedback in German", async () => {
    await loadConfig();
    await importPaths(["C:\\benchy.stl"]);
    expect(toasts()).toContain("success: 1 Modell geladen");
    useStore.setState({ toasts: [] });
    await importPaths(["C:\\a.stl", "C:\\b.stl"]);
    expect(toasts()).toContain("success: 2 Modelle geladen");
  });

  it("translates panel labels, counts and notes that used to be English-only", async () => {
    const [o] = seed("cube");
    useStore.getState().select([o.id]);
    await renderWithConfig(
      <>
        <TopBar onPickPrinter={() => {}} />
        <ModelView />
      </>,
    );
    expect(screen.getByRole("toolbar")).toHaveAccessibleName("Werkzeuge: Vorbereiten");
    // transform panel
    expect(screen.getByLabelText(/^Z \(Anhebung\)/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^Drehen X/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^Größe Y/)).toBeInTheDocument();
    expect(screen.getByText("12 Dreiecke · 8.00 ml")).toBeInTheDocument();
    expect(screen.getByTitle("cube (Doppelklick zum Umbenennen)")).toBeInTheDocument();
    // slice panel
    expect(screen.getByText("2.5 s · Boden 30 s × 4")).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Druckerstandard (goo)" })).toBeInTheDocument();
    useStore.getState().updateDevice("10.0.0.2", { connected: true });
    expect(await screen.findByLabelText("1 verbunden")).toBeInTheDocument();
    useStore.getState().setSliceProgress({ done: 3, total: 40 });
    expect(await screen.findByRole("progressbar")).toHaveTextContent("Slicen 3/40");
  });

  it("translates the preview, the send dialog summary and the keyboard help", async () => {
    const user = userEvent.setup();
    await loadConfig();
    seed({ name: "coin", size: [20, 20, 2] });
    const { sliceNow } = await import("../actions");
    await sliceNow(null);
    const { PreviewView } = await import("../views/PreviewView");
    render(<PreviewView />);
    expect(screen.getByRole("heading", { name: "Schicht 1" })).toBeInTheDocument();
    expect(screen.getByText(/Die Vorschau zeigt die Platte von oben/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "An Drucker senden" }));
    expect(screen.getByText("1.2 MB · 40 Schichten · .goo")).toBeInTheDocument();
    const { HelpDialog } = await import("../views/Onboarding");
    render(<HelpDialog onClose={() => {}} />);
    expect(screen.getByText("Strg+Z / Strg+Y")).toBeInTheDocument();
    expect(screen.getByText("Entf")).toBeInTheDocument();
  });

  it("the welcome dialog's back button means 'previous step', not 'rear'", async () => {
    const user = userEvent.setup();
    await loadConfig();
    render(<WelcomeDialog onClose={() => {}} />);
    await user.click(screen.getByRole("button", { name: "Weiter" }));
    expect(screen.getByRole("button", { name: "Zurück" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hinten" })).toBeNull();
  });

  it("the error screen is German", () => {
    function Boom(): never {
      throw new Error("kaputt");
    }
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const swallow = (e: ErrorEvent) => e.preventDefault();
    window.addEventListener("error", swallow);
    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    window.removeEventListener("error", swallow);
    err.mockRestore();
    expect(screen.getByRole("heading", { name: "In der Oberfläche ist ein Fehler aufgetreten." })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Oberfläche neu laden" })).toBeInTheDocument();
  });

  it("switching back to English stores the choice and reloads", async () => {
    const reload = vi.fn();
    vi.stubGlobal("location", { ...window.location, reload });
    setLang("en");
    expect(localStorage.getItem("openresin.lang")).toBe("en");
    expect(reload).toHaveBeenCalled();
    await waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
  });
});
