import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReviewCard } from "../../../types";
import { type ReviewSection, ReviewSections } from "./ReviewSections";
import { labelCard } from "./reviewHelpers";

const sections: ReviewSection[] = [
  { id: "needs_you", title: "Precisa de você", lanes: ["manual_required"], defaultOpen: true },
  { id: "archive", title: "Closed and merged", lanes: ["merged"], defaultOpen: false, history: true },
];
const card = (id: number, lane: string) =>
  ({ id, number: id, repoFullName: "example/repo", title: `PR ${id}`, author: "developer", lane }) as ReviewCard;

afterEach(() => {
  localStorage.clear();
  vi.unstubAllGlobals();
});

it("keeps what is collapsed during a search out of the saved sections", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  const render = (expandAll: boolean) =>
    act(async () =>
      root.render(
        <ReviewSections
          sections={sections}
          items={[card(1, "manual_required"), card(2, "merged")]}
          expandAll={expandAll}
          selectedId={null}
          attentionPrIds={new Set()}
          now={0}
          onSelect={() => {}}
        />,
      ),
    );
  const toggle = (id: string) =>
    container.querySelector<HTMLButtonElement>(`[data-section="${id}"] .review-section-toggle`)!;
  try {
    await render(true);
    expect(toggle("needs_you").getAttribute("aria-expanded")).toBe("true");
    expect(toggle("archive").getAttribute("aria-expanded")).toBe("true");
    await act(async () => toggle("needs_you").click());
    expect(toggle("needs_you").getAttribute("aria-expanded")).toBe("false");
    expect(localStorage.getItem("ocr.reviewSectionsOpen")).toBeNull();
    await render(false);
    expect(toggle("needs_you").getAttribute("aria-expanded")).toBe("true");
    expect(toggle("archive").getAttribute("aria-expanded")).toBe("false");
    await render(true);
    expect(toggle("needs_you").getAttribute("aria-expanded")).toBe("true");
  } finally {
    await act(async () => root.unmount());
  }
});

describe("labelCard", () => {
  it("says a published review had only suggestions", () => {
    expect(labelCard({ lane: "auto_posted", suggestionsOnly: true, questionsOnly: false })).toBe(
      "Sugestões publicadas",
    );
    expect(labelCard({ lane: "posted", suggestionsOnly: false, questionsOnly: false })).toBe("Achados publicados");
    expect(labelCard({ lane: "posted", suggestionsOnly: false, questionsOnly: true })).toBe("Perguntas publicadas");
    expect(labelCard({ lane: "ready", suggestionsOnly: true, questionsOnly: true })).toBe("Pronto para publicar");
  });
});
