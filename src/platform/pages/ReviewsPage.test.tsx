import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NuqsTestingAdapter } from "nuqs/adapters/testing";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { ReviewRun } from "../../types";
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

describe("review processes toggle", () => {
  it("shows restoration failures for the selected PR without offering to repost its review", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
    client.setQueryData(
      ["reviews", "board"],
      [
        {
          items: [
            {
              id: 1,
              number: 42,
              repoFullName: "example/repo",
              title: "Posted review",
              author: "developer",
              lane: "posted",
              headSha: "head",
            },
          ],
        },
      ],
    );
    client.setQueryData(
      ["pending"],
      [
        {
          id: 1,
          prId: 1,
          kind: "review_requests_restore_failed",
          status: "seen",
          message: "Check the requested reviewers on GitLab.",
        },
        { id: 2, prId: 2, kind: "review_requests_restore_failed", status: "unseen", message: "Other PR warning" },
      ],
    );
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
      expect(container.querySelector('[role="alert"]')?.textContent).toBe("Check the requested reviewers on GitLab.");
      expect(container.textContent).not.toContain("Other PR warning");
      expect(container.textContent).not.toContain("Retry publication");
      expect(setupApi.decideReview).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.unstubAllGlobals();
    }
  });

  it.each([true, false])("shows one initial loader, preferring the visible sidebar (wide: %s)", async (wide) => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("matchMedia", () => ({ matches: wide, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
    client.setQueryDefaults(["reviews", "board"], { enabled: true });
    let finish!: (value: Awaited<ReturnType<typeof setupApi.listReviewBoard>>) => void;
    vi.mocked(setupApi.listReviewBoard).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const container = document.createElement("div");
    const root = createRoot(container);
    const expectLoader = (sidebar: boolean) => {
      expect(container.querySelectorAll(".brand-loading")).toHaveLength(1);
      expect(
        container.querySelector(`${sidebar ? "#review-processes" : ".review-workspace-main"} .brand-loading`),
      ).not.toBeNull();
      if (sidebar)
        expect(container.querySelector(".review-workspace-main .brand-loading, .review-workspace-empty")).toBeNull();
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
      expectLoader(wide);
      await act(async () =>
        container.querySelector<HTMLButtonElement>(`button[aria-label="${wide ? "Hide" : "Show"} processes"]`)!.click(),
      );
      expectLoader(!wide);
      await act(async () => {
        finish([]);
      });
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      expect(container.querySelector(".brand-loading")).toBeNull();
      expect(container.textContent).toContain("Poll GitLab, then select a merge request");
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.unstubAllGlobals();
    }
  });

  it("groups reviews by today and sorts each group newest first", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    const safeToday = new Date();
    safeToday.setHours(12, 0, 0, 0);
    vi.useFakeTimers({ now: safeToday });
    const now = Date.now();
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
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
              title: "Older today",
              author: "developer",
              lane: "manual_required",
              headSha: "one",
              updatedAt: now - 120_000,
            },
            {
              id: 2,
              number: 43,
              repoFullName: "example/repo",
              title: "Newest today",
              author: "developer",
              lane: "manual_required",
              headSha: "two",
              updatedAt: now - 60_000,
            },
            {
              id: 3,
              number: 44,
              repoFullName: "example/repo",
              title: "Yesterday",
              author: "developer",
              lane: "manual_required",
              headSha: "three",
              updatedAt: yesterday.getTime(),
            },
          ],
        },
      ],
    );
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
      const needsYou = container.querySelector<HTMLElement>('[data-section="needs_you"]')!;
      expect(
        [...needsYou.querySelectorAll<HTMLElement>(".review-process-item-pr-title")].map((item) => item.textContent),
      ).toEqual(["Newest today", "Older today", "Yesterday"]);
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.useRealTimers();
      vi.unstubAllGlobals();
    }
  });

  it("moves a skipped review out of Needs you after the board refresh", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    const client = new QueryClient({
      defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } },
    });
    const board = [
      {
        items: [
          {
            id: 1,
            number: 42,
            repoFullName: "example/repo",
            title: "Skip this review",
            author: "developer",
            lane: "ready",
            headSha: "head",
          },
          {
            id: 2,
            number: 43,
            repoFullName: "example/repo",
            title: "Still needs review",
            author: "developer",
            lane: "manual_required",
            headSha: "other",
          },
        ],
      },
    ];
    client.setQueryData(["reviews", "board"], board);
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
      const panel = container.querySelector<HTMLElement>("#review-processes")!;
      const section = (id: string) => panel.querySelector<HTMLElement>(`[data-section="${id}"]`)!;
      expect(section("needs_you").textContent).toContain("Skip this review");
      expect(section("needs_you").textContent).toContain("Still needs review");
      client.setQueryData(
        ["reviews", "board"],
        [
          {
            items: [
              {
                id: 1,
                number: 42,
                repoFullName: "example/repo",
                title: "Skip this review",
                author: "developer",
                lane: "skipped",
                headSha: "head",
                resultState: "skipped",
                resultObservedHeadSha: "head",
              },
              {
                id: 2,
                number: 43,
                repoFullName: "example/repo",
                title: "Still needs review",
                author: "developer",
                lane: "manual_required",
                headSha: "other",
              },
            ],
          },
        ],
      );
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      expect(section("needs_you").querySelector(".review-section-count")?.textContent).toBe("1");
      expect(section("needs_you").textContent).not.toContain("Skip this review");
      expect(section("needs_you").textContent).toContain("Still needs review");
      const ignored = section("ignored");
      expect(ignored.querySelector(".review-section-count")?.textContent).toBe("1");
      // History sections start collapsed, unless they hold the selected PR.
      if (!ignored.querySelector(".review-section-items"))
        await act(async () => ignored.querySelector<HTMLButtonElement>(".review-section-toggle")!.click());
      expect(ignored.textContent).toContain("Skip this review");
      expect(ignored.textContent).toContain("Skipped");
      expect(setupApi.decideReview).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.unstubAllGlobals();
    }
  });
});

it("selects the merge request a ?pr= link names, then drops the link from the URL", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  const card = (id: number, title: string, lane: string) => ({
    id,
    number: 40 + id,
    repoFullName: "example/repo",
    title,
    author: "developer",
    lane,
    headSha: `head-${id}`,
    updatedAt: Date.now() - id,
  });
  client.setQueryData(
    ["reviews", "board"],
    [{ items: [card(1, "First in line", "manual_required"), card(2, "Linked from usage", "ready")] }],
  );
  const onUrlUpdate = vi.fn();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <NuqsTestingAdapter searchParams="?pr=2" onUrlUpdate={onUrlUpdate}>
            <ReviewsPage />
          </NuqsTestingAdapter>
        </QueryClientProvider>,
      ),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 100));
    });
    const pressed = [...container.querySelectorAll('button[aria-pressed="true"]')].map((button) => button.textContent);
    expect(pressed).toHaveLength(1);
    expect(pressed[0]).toContain("Linked from usage");
    expect(onUrlUpdate).toHaveBeenCalled();
    expect(onUrlUpdate.mock.lastCall?.[0].searchParams.has("pr")).toBe(false);
  } finally {
    await act(async () => root.unmount());
    client.clear();
    vi.unstubAllGlobals();
  }
});
