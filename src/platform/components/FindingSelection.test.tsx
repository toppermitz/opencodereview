import { act, type ComponentProps, useState } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { FindingAuditView, FindingCandidate } from "../../types";
import { FindingSelection } from "./FindingSelection";

const candidate = (
  id: string,
  aiDecision: FindingCandidate["aiDecision"],
  blockedReason: string | null = null,
): FindingCandidate => ({
  id,
  aiDecision,
  blockedReason,
  selected: aiDecision === "kept",
  defaultSelected: aiDecision === "kept",
  origin: "chunk",
  chunkId: 1,
  aiReason: `${id} evidence`,
  body: `[Achado] ${id} title\n\n${id} explanation`,
  originalBody: null,
  comment: { path: "src/example.ts", line: 1 },
});
const view: FindingAuditView = {
  runId: 10,
  prId: 1,
  revision: 2,
  requiresConfirmation: true,
  selectionSaved: false,
  editable: true,
  audit: {
    headSha: "abc",
    complete: true,
    originalBody: "Original summary",
    candidates: [
      candidate("kept", "kept"),
      candidate("discarded", "discarded"),
      candidate("blocked", "kept", "Invalid anchor"),
      candidate("unknown", "unmapped"),
    ],
  },
};
const props: ComponentProps<typeof FindingSelection> = {
  view,
  selectedIds: ["kept"],
  loading: false,
  busy: false,
  dirty: false,
  conflict: false,
  error: null,
  onToggle: () => undefined,
  onSave: () => undefined,
  onReset: () => undefined,
};
const render = (overrides: Partial<typeof props> = {}) =>
  renderToStaticMarkup(
    <FindingSelection {...props} {...overrides}>
      <p>Legacy final output</p>
    </FindingSelection>,
  );
const chip = (container: HTMLElement, label: string) =>
  [...container.querySelectorAll<HTMLButtonElement>(".finding-group-chip")].find((button) =>
    button.textContent?.startsWith(label),
  )!;
const click = async (element: HTMLElement) => act(async () => element.click());

afterEach(() => vi.unstubAllGlobals());

function Harness({ overrides }: { overrides: Partial<typeof props> }) {
  const [selectedIds, setSelectedIds] = useState(overrides.selectedIds ?? props.selectedIds);
  return (
    <FindingSelection
      {...props}
      {...overrides}
      selectedIds={selectedIds}
      onToggle={(id) => {
        overrides.onToggle?.(id);
        setSelectedIds((current) =>
          current.includes(id) ? current.filter((value) => value !== id) : [...current, id],
        );
      }}
    >
      <p>Legacy final output</p>
    </FindingSelection>
  );
}

async function interact(overrides: Partial<typeof props>, test: (container: HTMLElement) => Promise<void>) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () => root.render(<Harness overrides={overrides} />));
    await test(container);
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
}

describe("finding selection groups", () => {
  it("defaults to eligible findings and renders only that group", () => {
    const html = render();
    expect(html).not.toContain('role="tablist"');
    expect(html).toContain('data-finding-group="eligible"');
    expect(html).toContain("Achados elegíveis");
    expect(html).toContain("Descartados pela IA");
    expect(html).toContain("Bloqueados pela validação");
    expect(html).toContain("kept explanation");
    expect(html).not.toContain("discarded explanation");
    expect(html).not.toContain("Legacy final output");
    expect(html).not.toContain("<select");
  });

  it("shows each finding under its own title, without the GitHub-only lines or redundant tags", () => {
    const html = render();
    expect(html).not.toContain("Revisão automatizada por OpenCodeReview");
    expect(html.match(/kept title/g)).toHaveLength(1);
    expect(html).not.toContain("AI: selected");
    expect(html).toContain("src/example.ts:1 · Chunk 1");
    expect(html).not.toContain("· chunk");
  });

  it("folds the original AI summary below the findings", () => {
    const html = render();
    expect(html).toMatch(/<details class="finding-original-review[^"]*"><summary>Resumo original da IA<\/summary>/);
  });

  it("switches groups, preserves choices across them and keeps blocked findings disabled", async () => {
    const onToggle = vi.fn();
    const onSave = vi.fn();
    const onReset = vi.fn();
    await interact({ onToggle, onSave, onReset }, async (container) => {
      expect(chip(container, "Achados elegíveis").getAttribute("aria-pressed")).toBe("true");
      await click(chip(container, "Descartados pela IA"));
      expect(chip(container, "Descartados pela IA").getAttribute("aria-pressed")).toBe("true");
      expect(chip(container, "Achados elegíveis").getAttribute("aria-pressed")).toBe("false");
      const discarded = container.querySelector<HTMLInputElement>('[aria-label="Include finding discarded"]')!;
      expect(discarded.disabled).toBe(false);
      expect(discarded.checked).toBe(false);
      expect(container.textContent).toContain("discarded evidence");
      expect(container.textContent).toContain("IA: discarded");
      await click(discarded);
      expect(onToggle).toHaveBeenCalledExactlyOnceWith("discarded");
      await click(chip(container, "Bloqueados pela validação"));
      expect(container.querySelector<HTMLInputElement>('[aria-label="Include finding blocked"]')!.disabled).toBe(true);
      expect(container.textContent).toContain("Invalid anchor");
      await click(chip(container, "Descartados pela IA"));
      expect(container.querySelector<HTMLInputElement>('[aria-label="Include finding discarded"]')!.checked).toBe(true);
      expect(container.textContent).toContain("Sobrescrita manual");
      expect(container.querySelectorAll(".finding-tab-panel")).toHaveLength(1);
      expect(chip(container, "Descartados pela IA").getAttribute("aria-controls")).toBe(
        container.querySelector(".finding-tab-panel")!.id,
      );
      expect(onSave).not.toHaveBeenCalled();
      expect(onReset).not.toHaveBeenCalled();
    });
  });

  it("keeps questions optional and exposes unmapped and pending evidence in their own tabs", async () => {
    const question = {
      ...candidate("question", "kept"),
      kind: "question" as const,
      body: "[Pergunta] Confirm rollout",
      selected: false,
      defaultSelected: false,
    };
    await interact(
      {
        view: {
          ...view,
          audit: {
            ...view.audit!,
            candidates: [
              candidate("kept", "kept"),
              question,
              candidate("unknown", "unmapped"),
              candidate("pending", "pending"),
            ],
          },
        },
      },
      async (container) => {
        expect(container.textContent).toContain("1 achado · 0 perguntas selecionadas");
        expect(container.textContent).toContain("sozinhas não justificam Solicitar mudanças");
        await click(chip(container, "Perguntas — publicação opcional"));
        expect(container.querySelector<HTMLInputElement>('[aria-label="Include question question"]')!.checked).toBe(
          false,
        );
        await click(chip(container, "Decisão indisponível"));
        expect(container.textContent).toContain("unknown evidence");
        await click(chip(container, "Aguardando revisão final"));
        expect(container.textContent).toContain("pending evidence");
      },
    );
  });

  it("keeps historical final output and explains missing discard history", () => {
    const html = render({ view: { ...view, editable: false, audit: null } });
    expect(html).toContain("Legacy final output");
    expect(html).toContain('data-finding-group="unavailable"><summary>');
    expect(html).toContain("Histórico indisponível");
    expect(html).not.toContain("0 discarded");
    expect(html).not.toContain("Save selection");
  });

  it("does not ask for confirmation when no candidates were generated", () => {
    const html = render({ view: { ...view, audit: { ...view.audit!, candidates: [] } }, selectedIds: [] });
    expect(html).toContain("Escolha uma decisão de revisão abaixo");
    expect(html).not.toContain("Save the empty selection");
    expect(html).not.toContain("Confirmar achados");
    expect(html).not.toContain("finding-selection-actions");
  });

  it.each([{ conflict: true }, { busy: true }, { error: "Audit failed" }, { view: { ...view, editable: false } }])(
    "blocks editing across groups for unavailable or read-only states %j",
    async (overrides) => {
      await interact(overrides, async (container) => {
        expect(container.querySelector<HTMLInputElement>('[aria-label="Include finding kept"]')!.disabled).toBe(true);
        await click(chip(container, "Descartados pela IA"));
        expect(container.querySelector<HTMLInputElement>('[aria-label="Include finding discarded"]')!.disabled).toBe(
          true,
        );
      });
    },
  );
});
