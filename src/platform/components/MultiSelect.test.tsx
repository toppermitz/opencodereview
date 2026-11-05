import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MultiSelect } from "./MultiSelect";

const dialogMethods = ["showModal", "close"].map(
  (name) => [name, Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, name)] as const,
);
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true,
    value: function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
      this.querySelector("input")?.focus();
    },
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true,
    value: function (this: HTMLDialogElement) {
      this.removeAttribute("open");
    },
  });
});
afterEach(() => {
  for (const [name, descriptor] of dialogMethods) {
    if (descriptor) Object.defineProperty(HTMLDialogElement.prototype, name, descriptor);
    else Reflect.deleteProperty(HTMLDialogElement.prototype, name);
  }
  vi.unstubAllGlobals();
});

const repositories = ["BlueRiverTechnology/brt-prod-fleet-triage", "BlueRiverTechnology/brt-prod-version-manager"];
function Fixture() {
  const [selected, setSelected] = useState(new Set<string>());
  return (
    <MultiSelect
      label="Repositórios"
      placeholder="Filtrar repositórios"
      presentation="dialog"
      options={repositories.map((value) => ({ value, label: value }))}
      selected={selected}
      onToggle={(value) =>
        setSelected((current) => {
          const next = new Set(current);
          if (next.has(value)) next.delete(value);
          else next.add(value);
          return next;
        })
      }
    />
  );
}

it("searches full repository names, keeps multiple selections and restores focus when closed", async () => {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () => root.render(<Fixture />));
    const trigger = container.querySelector<HTMLButtonElement>(".settings-multiselect-trigger")!;
    trigger.focus();
    await act(async () => trigger.click());
    const dialog = container.querySelector<HTMLDialogElement>("dialog[open]")!;
    expect(dialog).not.toBeNull();
    expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");
    expect(container.querySelector(".settings-multiselect-popover")).toBeNull();
    const search = dialog.querySelector<HTMLInputElement>('[aria-label="Buscar repositórios"]')!;
    expect(document.activeElement).toBe(search);
    const options = () => [...dialog.querySelectorAll<HTMLButtonElement>(".settings-multiselect-option")];
    expect(options().map((option) => option.textContent)).toEqual(repositories);
    for (const option of options()) await act(async () => option.click());
    expect(dialog.open).toBe(true);
    expect(options().every((option) => option.getAttribute("aria-pressed") === "true")).toBe(true);
    expect(dialog.querySelector("footer span")?.textContent).toBe("2 selecionados");
    const searchFor = async (value: string) =>
      act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(search, value);
        search.dispatchEvent(new Event("input", { bubbles: true }));
      });
    await searchFor("VERSION-MANAGER");
    expect(options().map((option) => option.textContent)).toEqual([repositories[1]]);
    await act(async () => options()[0].click());
    expect(dialog.querySelector("footer span")?.textContent).toBe("1 selecionado");
    await searchFor("missing-repository");
    expect(dialog.querySelector(".settings-empty-inline")?.textContent).toBe("Nenhuma correspondência");
    await searchFor("");
    await act(async () => dialog.querySelector<HTMLButtonElement>("footer button")!.click());
    expect(container.querySelector("dialog")).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(trigger.textContent).toContain(repositories[0]);
    for (const event of [
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      new Event("cancel", { cancelable: true }),
      new MouseEvent("click", { clientX: -1, clientY: -1, bubbles: true }),
    ]) {
      await act(async () => trigger.click());
      await act(async () => container.querySelector("dialog")!.dispatchEvent(event));
      expect(container.querySelector("dialog")).toBeNull();
      expect(document.activeElement).toBe(trigger);
      expect(trigger.textContent).toContain(repositories[0]);
    }
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
