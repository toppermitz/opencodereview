import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import type { Editor } from "@tiptap/core";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { SkillVersion } from "../../types";
import { SkillsPage } from "./SkillsPage";

vi.mock("@tanstack/react-router", async (original) => ({
  ...(await original<typeof import("@tanstack/react-router")>()),
  useBlocker: vi.fn(),
}));
vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listSkillVersions: vi.fn(),
    getSkillVersionContent: vi.fn(),
    createSkillVersion: vi.fn(),
    setActiveSkillVersion: vi.fn(),
    deleteSkillVersion: vi.fn(),
  },
}));

const version = (id: number, active = false): SkillVersion => ({
  id,
  version: id,
  active,
  note: null,
  charCount: 100,
  createdAt: 1720000000000,
});
const original = "# Default skill\n\nKeep **evidence**.\n";
let versions: SkillVersion[];
let contents: Record<number, string>;
let host: HTMLDivElement;
let root: Root;
let client: QueryClient;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.clearAllMocks();
  versions = [version(0), version(1, true)];
  contents = { 0: original, 1: "# Learned\n\nLearned rules." };
  vi.mocked(setupApi.listSkillVersions).mockImplementation(async () => versions);
  vi.mocked(setupApi.getSkillVersionContent).mockImplementation(async (id) => contents[id]);
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } },
  });
  client.setQueryData(["skills", "versions"], versions);
  for (const [id, content] of Object.entries(contents)) client.setQueryData(["skills", Number(id), "content"], content);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("loads versions only in the sidebar before loading the selected skill", async () => {
  client.removeQueries({ queryKey: ["skills"] });
  let finishVersions!: (value: SkillVersion[]) => void;
  let finishContent!: (value: string) => void;
  vi.mocked(setupApi.listSkillVersions).mockImplementation(
    () =>
      new Promise((resolve) => {
        finishVersions = resolve;
      }),
  );
  vi.mocked(setupApi.getSkillVersionContent).mockImplementation(
    () =>
      new Promise((resolve) => {
        finishContent = resolve;
      }),
  );
  await render();
  expect(host.querySelectorAll(".brand-loading")).toHaveLength(1);
  expect(host.querySelector(".skills-version-list .brand-loading")).not.toBeNull();
  expect(host.querySelector(".skill-view-panel")?.textContent).toBe("");
  await act(async () => finishVersions(versions));
  await settle();
  expect(host.querySelectorAll(".brand-loading")).toHaveLength(1);
  expect(host.querySelector(".skill-view-panel .brand-loading")).not.toBeNull();
  await act(async () => finishContent(original));
  await settle();
});

async function render() {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <SkillsPage />
      </QueryClientProvider>,
    ),
  );
  await act(async () => {
    await vi.dynamicImportSettled();
  });
  await settle();
}
async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });
}
function button(label: string) {
  const found = [...host.querySelectorAll("button")].find(
    (element) => element.getAttribute("aria-label") === label || element.textContent === label,
  );
  if (!found) throw new Error(`Button not found: ${label}`);
  return found;
}
async function click(label: string) {
  await act(async () => button(label).click());
  await settle();
}
async function changeSource(value: string) {
  const input = host.querySelector("textarea")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function configureSave() {
  vi.mocked(setupApi.createSkillVersion).mockImplementation(async (content) => {
    const saved = version(2, true);
    versions = [...versions.map((item) => ({ ...item, active: false })), saved];
    contents[2] = content;
    return saved;
  });
}

describe("Skills workspace", () => {
  it("exposes editor views as tabs with selection, panel labels and keyboard navigation", async () => {
    await render();
    const tabs = host.querySelector('[role="tablist"][aria-label="Visão do editor"]');
    expect(tabs).not.toBeNull();
    expect(tabs!.querySelectorAll('[role="tab"]')).toHaveLength(3);
    for (const label of ["Markdown", "Diff do padrão", "Visual"]) {
      await click(label);
      const selected = button(label);
      expect(selected.getAttribute("aria-selected")).toBe("true");
      expect(selected.tabIndex).toBe(0);
      expect(tabs!.querySelectorAll('[aria-selected="true"]')).toHaveLength(1);
      const panel = host.querySelector('[role="tabpanel"]');
      expect(panel?.id).toBe(selected.getAttribute("aria-controls"));
      expect(panel?.getAttribute("aria-labelledby")).toBe(selected.id);
    }
    button("Visual").focus();
    for (const [key, label] of [
      ["ArrowRight", "Markdown"],
      ["End", "Diff do padrão"],
      ["ArrowRight", "Visual"],
      ["ArrowLeft", "Diff do padrão"],
      ["Home", "Visual"],
    ]) {
      await act(async () =>
        document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })),
      );
      expect(document.activeElement).toBe(button(label));
      expect(button(label).getAttribute("aria-selected")).toBe("true");
    }
    expect(button("Salvar nova versão").disabled).toBe(true);
  });

  it("opens the active version, selects Default id 0 and keeps untouched Markdown exact across views", async () => {
    await render();
    expect(button("Abrir v1").getAttribute("aria-pressed")).toBe("true");
    await click("Abrir Padrão");
    expect(button("Abrir Padrão").getAttribute("aria-pressed")).toBe("true");
    expect(host.querySelector('[aria-label="Skill visual editor"] h1')?.textContent).toBe("Default skill");
    await click("Markdown");
    expect(host.querySelector("textarea")?.value).toBe(original);
    await click("Visual");
    await click("Markdown");
    expect(host.querySelector("textarea")?.value).toBe(original);
    expect(button("Salvar nova versão").disabled).toBe(true);
    expect(host.querySelector("button button")).toBeNull();
  });

  it("saves a Markdown draft as a new active version without overwriting the original", async () => {
    configureSave();
    await render();
    await click("Abrir Padrão");
    await click("Markdown");
    const draft = "# My skill\n\n- **Check evidence**\n";
    await changeSource(draft);
    await click("Visual");
    expect(host.querySelector('[aria-label="Skill visual editor"] h1')?.textContent).toBe("My skill");
    await click("Diff do padrão");
    expect(host.textContent).toContain("# My skill");
    await click("Salvar nova versão");
    expect(vi.mocked(setupApi.createSkillVersion).mock.calls[0][0]).toBe(draft);
    expect(contents[0]).toBe(original);
    expect(button("Abrir v2").getAttribute("aria-pressed")).toBe("true");
    expect(host.textContent).toContain("v2 salva e ativada.");
    expect(button("Salvar nova versão").disabled).toBe(true);
  });

  it("guards unsaved edits on version switches, navigation and reset", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await render();
    await click("Markdown");
    await changeSource("# Unsaved");
    await click("Abrir Padrão");
    expect(confirm).toHaveBeenCalled();
    expect(host.querySelector("textarea")?.value).toBe("# Unsaved");
    const blocker = vi.mocked(useBlocker).mock.calls.at(-1)?.[0] as unknown as {
      enableBeforeUnload: boolean;
      shouldBlockFn: () => boolean;
    };
    expect(blocker.enableBeforeUnload).toBe(true);
    expect(blocker.shouldBlockFn()).toBe(true);
    await click("Descartar mudanças");
    expect(host.querySelector("textarea")?.value).toBe("# Unsaved");
    confirm.mockReturnValue(true);
    await click("Descartar mudanças");
    expect(host.querySelector("textarea")?.value).toBe(contents[1]);
    expect(button("Salvar nova versão").disabled).toBe(true);
  });

  it("retains the draft on save failure and blocks empty versions", async () => {
    vi.mocked(setupApi.createSkillVersion).mockRejectedValue(new Error("Save unavailable"));
    await render();
    await click("Markdown");
    await changeSource("  ");
    expect(button("Salvar nova versão").disabled).toBe(true);
    await changeSource("# Retry me");
    await click("Salvar nova versão");
    expect(host.textContent).toContain("Save unavailable");
    expect(host.querySelector("textarea")?.value).toBe("# Retry me");
    expect(button("Salvar nova versão").disabled).toBe(false);
  });

  it("freezes editing and selection while a save is pending", async () => {
    let finish: (value: SkillVersion) => void = () => undefined;
    vi.mocked(setupApi.createSkillVersion).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    await render();
    await click("Markdown");
    await changeSource("# In flight");
    await click("Salvar nova versão");
    expect(host.querySelector("textarea")?.disabled).toBe(true);
    expect(button("Abrir Padrão").disabled).toBe(true);
    expect(button("Visual").disabled).toBe(true);
    expect(button("Salvando…").disabled).toBe(true);
    versions = [...versions.map((item) => ({ ...item, active: false })), version(2, true)];
    contents[2] = "# In flight";
    await act(async () => finish(version(2, true)));
    await settle();
    expect(button("Abrir v2").getAttribute("aria-pressed")).toBe("true");
  });

  it("tracks real visual formatting and restores the exact original on undo", async () => {
    await render();
    await click("Abrir Padrão");
    const { editor } = host.querySelector<HTMLElement & { editor: Editor }>('[aria-label="Skill visual editor"]')!;
    await act(async () => {
      editor.commands.selectAll();
      editor.commands.toggleBold();
    });
    expect(button("Bold").getAttribute("aria-pressed")).toBe("true");
    expect(button("Salvar nova versão").disabled).toBe(false);
    await act(async () => {
      editor.commands.undo();
    });
    expect(button("Salvar nova versão").disabled).toBe(true);
    await click("Markdown");
    expect(host.querySelector("textarea")?.value).toBe(original);
  });

  it("activates saved versions and requires confirmation before deleting snapshots", async () => {
    vi.mocked(setupApi.setActiveSkillVersion).mockImplementation(async (id) => {
      versions = versions.map((item) => ({ ...item, active: item.id === id }));
      return versions;
    });
    vi.mocked(setupApi.deleteSkillVersion).mockImplementation(async (id) => {
      versions = versions.filter((item) => item.id !== id);
      return versions;
    });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    await render();
    await click("Abrir Padrão");
    await click("Usar versão salva");
    expect(vi.mocked(setupApi.setActiveSkillVersion).mock.calls[0][0]).toBe(0);
    expect(host.textContent).toContain("Padrão está ativa");
    expect(host.textContent).not.toContain("Excluir versão");
    await click("Abrir v1");
    await click("Excluir versão");
    expect(setupApi.deleteSkillVersion).not.toHaveBeenCalled();
    confirm.mockReturnValue(true);
    await click("Excluir versão");
    expect(vi.mocked(setupApi.deleteSkillVersion).mock.calls[0][0]).toBe(1);
    expect(button("Abrir Padrão").getAttribute("aria-pressed")).toBe("true");
  });

  it("reports content errors instead of presenting an editable empty skill", async () => {
    client.removeQueries({ queryKey: ["skills", 0, "content"] });
    vi.mocked(setupApi.getSkillVersionContent).mockRejectedValue(new Error("Content unavailable"));
    await render();
    await click("Abrir Padrão");
    expect(host.textContent).toContain("Content unavailable");
    expect(button("Salvar nova versão").disabled).toBe(true);
    expect(host.querySelector('[aria-label="Skill visual editor"]')).toBeNull();
  });
});
