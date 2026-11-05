import { readFileSync } from "node:fs";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NuqsTestingAdapter } from "nuqs/adapters/testing";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { FindingAuditView, ReviewRun } from "../../types";
import { ReviewsPage } from "./ReviewsPage";

const reviewStream = vi.hoisted(() => ({ run: null as ReviewRun | null }));
vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listReviewBoard: vi.fn(),
    listPendingActions: vi.fn(),
    pollStatus: vi.fn(),
    gitlabAuthStatus: vi.fn(),
    markPrReviewSeen: vi.fn().mockResolvedValue(undefined),
    getReviewFindingAudit: vi.fn(),
    saveReviewFindingSelection: vi.fn(),
    startReview: vi.fn().mockResolvedValue(undefined),
    decideReview: vi.fn(),
  },
}));
vi.mock("../hooks/useReviewRunStream", () => ({
  useReviewRunStream: () => ({ data: reviewStream.run, streamState: "idle", isLoading: false }),
}));
vi.mock("../components/ReviewFlow", () => ({
  ReviewFlow: ({
    onSelectView,
    reviewDetails,
  }: {
    onSelectView: (view: string) => void;
    reviewDetails?: ReactNode;
  }) => (
    <div>
      Process canvas
      <button type="button" onClick={() => onSelectView("overview")}>
        View review result
      </button>
      {reviewDetails}
    </div>
  ),
}));

const dialogMethods = ["showModal", "close"].map(
  (name) => [name, Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, name)] as const,
);
beforeEach(() => {
  reviewStream.run = null;
  vi.clearAllMocks();
  vi.mocked(setupApi.getReviewFindingAudit).mockRejectedValue(new Error("Audit unavailable in this fixture"));
  vi.mocked(setupApi.saveReviewFindingSelection).mockReset();
  vi.mocked(setupApi.decideReview).mockReset().mockResolvedValue(undefined);
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true,
    value: function (this: HTMLDialogElement) {
      this.setAttribute("open", "");
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
  localStorage.clear();
  for (const [name, descriptor] of dialogMethods) {
    if (descriptor) Object.defineProperty(HTMLDialogElement.prototype, name, descriptor);
    else Reflect.deleteProperty(HTMLDialogElement.prototype, name);
  }
});

describe("review result decisions", () => {
  it("re-runs a read-only pending question only after an explicit click without forcing full", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    reviewStream.run = {
      id: 1522,
      prId: 1,
      status: "preview",
      headSha: "head",
      createdAt: 1000,
      finishedAt: 2000,
      reviewBody: "[Question] Confirm ownership",
    } as ReviewRun;
    const client = new QueryClient({
      defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } },
    });
    client.setQueryData(
      ["reviews", "board"],
      [
        {
          items: [
            {
              id: 1,
              number: 1181,
              repoFullName: "example/repo",
              title: "Pending question",
              author: "developer",
              lane: "ready",
              headSha: "head",
            },
          ],
        },
      ],
    );
    client.setQueryData(["reviews", 1, "findingAudit", 1522, "preview"], {
      runId: 1522,
      prId: 1,
      revision: 1,
      requiresConfirmation: true,
      selectionSaved: false,
      editable: false,
      audit: { headSha: "head", complete: true, originalBody: "Question", candidates: [] },
    });
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () =>
        root.render(
          <QueryClientProvider client={client}>
            <NuqsTestingAdapter>
              <ReviewsPage />
            </NuqsTestingAdapter>
          </QueryClientProvider>,
        ),
      );
      await act(async () =>
        [...container.querySelectorAll("button")]
          .find((button) => button.textContent === "View review result")!
          .click(),
      );
      expect(setupApi.startReview).not.toHaveBeenCalled();
      const rerun = [...container.querySelectorAll("button")].find((button) => button.textContent === "Re-run review")!;
      expect(rerun.disabled).toBe(false);
      expect(rerun.title).toContain("Consumes model usage");
      await act(async () => {
        rerun.click();
        await vi.waitFor(() => expect(setupApi.startReview).toHaveBeenCalledExactlyOnceWith(1, false));
      });
      expect(setupApi.decideReview).not.toHaveBeenCalled();
      expect(setupApi.saveReviewFindingSelection).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.unstubAllGlobals();
    }
  });

  it.each([
    ["mark_reviewed", "Mark reviewed"],
    ["approve_post", "Approve"],
    ["skip", "Skip"],
  ] as const)(
    "prepares an empty selection only on the explicit %s decision and uses the saved revision",
    async (decision, label) => {
      vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
      vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
      reviewStream.run = {
        id: 10,
        prId: 1,
        status: "preview",
        headSha: "head",
        inputTokens: 100,
        outputTokens: 20,
        createdAt: 1000,
        finishedAt: 2000,
        reviewBody: "No findings.",
      } as ReviewRun;
      const initial: FindingAuditView = {
        runId: 10,
        prId: 1,
        revision: 1,
        requiresConfirmation: true,
        selectionSaved: false,
        editable: true,
        audit: { headSha: "head", complete: true, originalBody: "No findings.", candidates: [] },
      };
      let current = { ...initial, revision: 2 };
      vi.mocked(setupApi.getReviewFindingAudit).mockImplementation(async () => current);
      vi.mocked(setupApi.saveReviewFindingSelection).mockImplementation(async (_prId, _runId, revision) => {
        current = { ...current, revision: revision + 1, selectionSaved: true };
        return current;
      });
      vi.mocked(setupApi.decideReview).mockImplementation(async (_prId, _decision, _runId, revision) => {
        if (!current.selectionSaved || revision !== current.revision)
          throw new Error("Confirm and save the current finding selection before deciding this review.");
      });
      const client = new QueryClient({
        defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } },
      });
      client.setQueryData(
        ["reviews", "board"],
        [
          {
            items: [
              {
                id: 1,
                number: 42,
                repoFullName: "example/repo",
                title: "Empty review",
                author: "developer",
                lane: "ready",
                headSha: "head",
              },
            ],
          },
        ],
      );
      client.setQueryData(["reviews", 1, "findingAudit", 10, "preview"], initial);
      const container = document.createElement("div");
      const root = createRoot(container);
      try {
        await act(async () =>
          root.render(
            <QueryClientProvider client={client}>
              <NuqsTestingAdapter>
                <ReviewsPage />
              </NuqsTestingAdapter>
            </QueryClientProvider>,
          ),
        );
        await act(async () =>
          [...container.querySelectorAll("button")]
            .find((button) => button.textContent === "View review result")!
            .click(),
        );
        expect(container.querySelector(".finding-selection-actions")).toBeNull();
        const option = [...container.querySelectorAll<HTMLButtonElement>('[role="radio"]')].find(
          (button) => button.querySelector(".review-decision-option-label")?.textContent === label,
        )!;
        expect(option.disabled).toBe(false);
        await act(async () => option.click());
        const sections = container
          .querySelector('[aria-label="Review result sections"]')!
          .querySelectorAll<HTMLButtonElement>('[role="tab"]');
        await act(async () => sections[1].click());
        expect(container.querySelector<HTMLElement>(".review-decision-footer")!.hidden).toBe(true);
        await act(async () => sections[0].click());
        expect(container.querySelector<HTMLElement>(".review-decision-footer")!.hidden).toBe(false);
        expect(
          container.querySelector('[role="radio"][aria-checked="true"] .review-decision-option-label')?.textContent,
        ).toBe(label);
        expect(setupApi.saveReviewFindingSelection).not.toHaveBeenCalled();
        expect(setupApi.decideReview).not.toHaveBeenCalled();
        const apply = container.querySelector<HTMLButtonElement>(".review-decision-apply")!;
        expect(apply.disabled).toBe(false);
        await act(async () => {
          apply.click();
          await vi.waitFor(() =>
            expect(setupApi.saveReviewFindingSelection).toHaveBeenCalledExactlyOnceWith(1, 10, 2, []),
          );
        });
        expect(setupApi.decideReview).toHaveBeenCalledExactlyOnceWith(1, decision, 10, 3);
        expect(vi.mocked(setupApi.decideReview).mock.invocationCallOrder[0]).toBeGreaterThan(
          vi.mocked(setupApi.saveReviewFindingSelection).mock.invocationCallOrder[0],
        );
      } finally {
        await act(async () => root.unmount());
        client.clear();
        vi.unstubAllGlobals();
      }
    },
  );

  it.each([false, true])(
    "defaults to Findings and preserves its tabs and choices across Details (editable: %s)",
    async (editable) => {
      vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
      vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
      const status = editable ? "preview" : "posted";
      reviewStream.run = {
        id: 10,
        prId: 1,
        status,
        headSha: "head",
        inputTokens: 1400,
        outputTokens: 220,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        createdAt: 1000,
        finishedAt: 2000,
        reviewBody: "Summary",
      } as ReviewRun;
      const client = new QueryClient({
        defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } },
      });
      client.setQueryData(
        ["reviews", "board"],
        [
          {
            items: [
              {
                id: 1,
                number: 42,
                repoFullName: "example/repo",
                title: "Review",
                author: "developer",
                lane: editable ? "ready" : "posted",
                headSha: "head",
                resultState: "findings_posted",
                resultObservedHeadSha: "head",
                resultPublicationStatus: "posted",
              },
            ],
          },
        ],
      );
      const findingAudit: FindingAuditView = {
        runId: 10,
        prId: 1,
        revision: 1,
        editable,
        requiresConfirmation: true,
        selectionSaved: false,
        audit: {
          headSha: "head",
          complete: true,
          originalBody: "Original summary text",
          candidates: [
            {
              id: "kept",
              kind: "finding",
              origin: "chunk",
              chunkId: 1,
              aiDecision: "kept",
              aiReason: "Evidence",
              selected: true,
              defaultSelected: true,
              blockedReason: null,
              comment: { path: "src/a.ts", line: 1 },
              body: "[Finding] Keep the guard",
              originalBody: null,
            },
          ],
        },
      };
      client.setQueryData(["reviews", 1, "findingAudit", 10, status], findingAudit);
      const container = document.createElement("div");
      const style = document.createElement("style");
      style.textContent = [
        ...readFileSync("src/styles/components.css", "utf8").matchAll(
          /\.review-(?:overview-body|result-section|decision-footer)[^{}]*\{[^}]*\}/g,
        ),
      ]
        .map((match) => match[0])
        .join("\n");
      document.head.append(style);
      document.body.append(container);
      const root = createRoot(container);
      try {
        await act(async () =>
          root.render(
            <QueryClientProvider client={client}>
              <NuqsTestingAdapter>
                <ReviewsPage />
              </NuqsTestingAdapter>
            </QueryClientProvider>,
          ),
        );
        await act(async () =>
          [...container.querySelectorAll("button")]
            .find((button) => button.textContent === "View review result")!
            .click(),
        );
        const navigation = container.querySelector('[role="tablist"][aria-label="Review result sections"]')!;
        const tabs = [...navigation.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
        expect(tabs.map((tab) => tab.textContent)).toEqual(["Findings", "Details"]);
        expect(tabs[0].getAttribute("aria-selected")).toBe("true");
        const findings = container.querySelector<HTMLElement>('[data-review-result-section="findings"]')!;
        const details = container.querySelector<HTMLElement>('[data-review-result-section="details"]')!;
        const body = container.querySelector<HTMLElement>(".review-overview-body")!;
        expect(findings.hidden).toBe(false);
        expect(getComputedStyle(details).display).toBe("none");
        const choice = findings.querySelector<HTMLInputElement>('[aria-label="Include finding kept"]')!;
        if (editable) await act(async () => choice.click());
        const summary = findings.querySelector<HTMLDetailsElement>(".finding-original-review")!;
        expect(summary.querySelector("summary")?.textContent).toBe("Original AI summary");
        await act(async () => summary.querySelector("summary")!.click());
        body.scrollTop = 200;
        await act(async () => tabs[1].click());
        expect(body.scrollTop).toBe(0);
        expect(details.hidden).toBe(false);
        expect(getComputedStyle(findings).display).toBe("none");
        expect(body.getAttribute("aria-labelledby")).toBe(tabs[1].id);
        expect(tabs[1].getAttribute("aria-controls")).toBe(body.id);
        const runDetails = details.querySelector(".review-run-details")!;
        expect(runDetails.tagName).toBe("SECTION");
        expect(runDetails.querySelector(":scope > summary")).toBeNull();
        expect(runDetails.textContent).toContain("Run outcome");
        expect(runDetails.textContent).toContain("1.6k tokens");
        if (editable)
          expect(getComputedStyle(container.querySelector(".review-decision-footer")!).display).toBe("none");
        await act(async () => tabs[1].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true })));
        expect(document.activeElement).toBe(tabs[0]);
        expect(findings.hidden).toBe(false);
        expect(details.hidden).toBe(true);
        expect(summary.open).toBe(true);
        expect(summary.textContent).toContain("Original summary text");
        expect(findings.querySelector<HTMLInputElement>('[aria-label="Include finding kept"]')!.checked).toBe(
          !editable,
        );
        expect(setupApi.saveReviewFindingSelection).not.toHaveBeenCalled();
        expect(setupApi.decideReview).not.toHaveBeenCalled();
      } finally {
        await act(async () => root.unmount());
        client.clear();
        container.remove();
        style.remove();
        vi.unstubAllGlobals();
      }
    },
  );

  it.each([
    [true, false],
    [false, false],
    [true, true],
    [false, true],
  ])(
    "keeps icon controls inside the workspace and supports reopening (wide: %s, populated: %s)",
    async (wide, populated) => {
      vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
      vi.stubGlobal("matchMedia", () => ({ matches: wide, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
      const client = new QueryClient({
        defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } },
      });
      client.setQueryData(
        ["reviews", "board"],
        populated
          ? [
              {
                items: [
                  {
                    id: 1,
                    number: 42,
                    repoFullName: "example/repo",
                    title: "Review change",
                    author: "developer",
                    lane: "manual_required",
                    url: "https://github.com/example/repo/pull/42",
                  },
                  {
                    id: 2,
                    number: 43,
                    repoFullName: "example/repo",
                    title: "Completed change",
                    author: "developer",
                    authorAvatarUrl: "/avatars/developer.png",
                    lane: "reviewed_cleanly",
                    url: "https://github.com/example/repo/pull/43",
                  },
                  {
                    id: 3,
                    number: 44,
                    repoFullName: "other/repo",
                    title: "Other review",
                    author: "reviewer-two",
                    lane: "manual_required",
                    url: "https://github.com/other/repo/pull/44",
                  },
                ],
              },
            ]
          : [],
      );
      const container = document.createElement("div");
      const root = createRoot(container);
      const toggle = (label: string) => {
        const button = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
        expect(button).not.toBeNull();
        expect(button!.textContent).toBe("");
        expect(button!.title).toBe(label);
        expect(button!.getAttribute("aria-controls")).toBe("review-processes");
        expect(button!.closest(".review-workspace")).not.toBeNull();
        return button!;
      };
      try {
        await act(async () =>
          root.render(
            <QueryClientProvider client={client}>
              <NuqsTestingAdapter>
                <ReviewsPage />
              </NuqsTestingAdapter>
            </QueryClientProvider>,
          ),
        );
        expect(container.querySelector(".review-workspace-summary button")).toBeNull();
        if (populated) {
          const githubLink = container.querySelector<HTMLAnchorElement>(".review-workspace-gitlab-link");
          expect(githubLink?.getAttribute("href")).toBe("https://github.com/example/repo/pull/42");
          expect(githubLink?.target).toBe("_blank");
        }
        const panel = container.querySelector<HTMLElement>("#review-processes")!;
        expect(panel.hidden).toBe(!wide);
        if (populated) {
          expect(
            [...panel.querySelectorAll<HTMLElement>(".review-section")].map((section) => section.dataset.section),
          ).toEqual(["needs_you", "ai_working", "outposts", "ignored", "archive"]);
          const toggle = (id: string) =>
            panel.querySelector<HTMLButtonElement>(`[data-section="${id}"] .review-section-toggle`)!;
          expect(toggle("needs_you").getAttribute("aria-expanded")).toBe("true");
          expect(toggle("archive").disabled).toBe(true);
          expect(toggle("outposts").getAttribute("aria-expanded")).toBe("false");
          await act(async () => toggle("outposts").click());
          expect(toggle("outposts").getAttribute("aria-expanded")).toBe("true");
          expect(panel.querySelector(".review-process-filters")).toBeNull();
          await act(async () => panel.querySelector<HTMLButtonElement>('button[aria-label="Filter reviews"]')!.click());
          const filters = panel.querySelectorAll<HTMLButtonElement>(
            ".review-process-filters .settings-multiselect-trigger",
          );
          expect(filters).toHaveLength(2);
          await act(async () => filters[0].click());
          const peopleOptions = [...panel.querySelectorAll<HTMLButtonElement>(".settings-multiselect-option")];
          expect(peopleOptions).toHaveLength(2);
          const developerOption = peopleOptions.find(
            (option) => option.querySelector("b")?.textContent === "developer",
          )!;
          const avatar = developerOption.querySelector("img");
          expect(avatar?.getAttribute("src")).toBe("/avatars/developer.png");
          expect(developerOption.querySelector(".settings-multiselect-option-icon")?.getAttribute("aria-hidden")).toBe(
            "true",
          );
          await act(async () => avatar!.dispatchEvent(new Event("error")));
          expect(developerOption.querySelector("img")).toBeNull();
          expect(developerOption.querySelector(".settings-multiselect-option-icon")?.textContent).toBe("D");
          const personOption = peopleOptions.find(
            (option) => option.querySelector("b")?.textContent === "reviewer-two",
          );
          expect(personOption).not.toBeNull();
          expect(personOption!.querySelector(".settings-multiselect-option-icon")?.textContent).toBe("R");
          await act(async () => personOption!.click());
          expect(panel.querySelector(".review-process-item")?.textContent).toContain("Other review");
          await act(async () => filters[1].click());
          const repositoryDialog = panel.querySelector<HTMLDialogElement>("dialog[open]");
          expect(repositoryDialog).not.toBeNull();
          expect(repositoryDialog!.querySelector("h2")?.textContent).toBe("Repositories");
          const repositoryOption = [
            ...repositoryDialog!.querySelectorAll<HTMLButtonElement>(".settings-multiselect-option"),
          ].find((option) => option.textContent?.includes("other/repo"));
          expect(repositoryOption).not.toBeNull();
          await act(async () => repositoryOption!.click());
          expect(repositoryDialog!.open).toBe(true);
          expect(repositoryOption!.getAttribute("aria-pressed")).toBe("true");
          await act(async () =>
            repositoryDialog!.querySelector<HTMLButtonElement>('[aria-label="Close repositories"]')!.click(),
          );
          expect(panel.querySelector("dialog")).toBeNull();
          expect(panel.querySelector(".review-process-filters-clear")).not.toBeNull();
          await act(async () => panel.querySelector<HTMLButtonElement>(".review-process-filters-clear")!.click());
          expect(panel.querySelector(".review-process-filters-clear")).toBeNull();
        }
        if (!wide) await act(async () => toggle("Show processes").click());
        const close = toggle("Hide processes");
        expect(panel.contains(close)).toBe(true);
        expect(close.getAttribute("aria-expanded")).toBe("true");
        await act(async () => close.click());
        expect(panel.hidden).toBe(true);
        const open = toggle("Show processes");
        expect(open.getAttribute("aria-expanded")).toBe("false");
        expect(open.closest(".review-workspace-main")).not.toBeNull();
        await act(async () => open.click());
        expect(panel.hidden).toBe(false);
        expect(container.querySelector('[aria-label="Show processes"]')).toBeNull();
      } finally {
        await act(async () => root.unmount());
        client.clear();
        vi.unstubAllGlobals();
      }
    },
  );
});
