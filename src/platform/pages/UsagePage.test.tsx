import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { NuqsTestingAdapter, type OnUrlUpdateFunction } from "nuqs/adapters/testing";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { UsageGroupRow, UsagePrRow, UsageReport, UsageRun } from "../../types";
import { UsagePage } from "./UsagePage";
import { renderShareCard, shareCardData } from "./usage/shareCard";

vi.mock("@tanstack/react-router", () => ({
  Link: ({ children, to, search }: { children: ReactNode; to: string; search?: Record<string, unknown> }) => (
    <a href={`${to}${search ? `?${new URLSearchParams(search as Record<string, string>)}` : ""}`}>{children}</a>
  ),
}));
vi.mock("./usage/UsageChart", () => ({
  UsageChart: ({ action }: { action?: ReactNode }) => <div data-usage-chart>{action}</div>,
}));
vi.mock("./usage/shareCard", async (original) => ({
  ...(await original<typeof import("./usage/shareCard")>()),
  renderShareCard: vi.fn(async () => new Blob(["png"], { type: "image/png" })),
}));
vi.mock("../../services/setupApi", () => ({
  setupApi: { usageReport: vi.fn(), usagePrs: vi.fn(), usagePrRuns: vi.fn() },
}));

const tokens = (total: number) => ({
  tokens: total,
  inputTokens: total * 0.6,
  outputTokens: total * 0.3,
  cacheReadTokens: total * 2,
  cacheCreationTokens: total * 0.1,
});
const group = (key: string, label: string, total: number, provider: string | null = null): UsageGroupRow => ({
  ...tokens(total),
  key,
  label,
  provider,
  runs: 3,
  findings: 2,
  findingsFound: 4,
  medianDurationMs: 90_000,
});
const report = (overrides: Partial<UsageReport> = {}): UsageReport => ({
  days: 30,
  from: 0,
  to: 0,
  totals: {
    ...tokens(12_420_000),
    runs: 142,
    prs: 58,
    findings: 311,
    questions: 40,
    findingsFound: 350,
    questionsFound: 44,
    medianDurationMs: 252_000,
    reusedChunks: 38,
    chunks: 100,
    overheadTokens: 4_000_000,
    runsWithoutUsage: 2,
  },
  byDay: [],
  byRepo: [group("1", "acme/api", 10_000_000), group("2", "acme/web", 2_420_000)],
  byModel: [group("claude-opus", "claude-opus", 9_000_000, "claude"), group("unattributed", "", 3_420_000)],
  byAccount: [group("7", "work@example.com", 9_000_000, "claude"), group("unattributed", "", 3_420_000)],
  byProvider: [group("claude", "Claude", 9_000_000, "claude"), group("unattributed", "", 3_420_000)],
  repos: [
    { id: 1, fullName: "acme/api" },
    { id: 2, fullName: "acme/web" },
  ],
  hasRunning: false,
  ...overrides,
});
const pr: UsagePrRow = {
  ...tokens(2_310_000),
  prId: 412,
  repo: "acme/api",
  number: 12,
  title: "Refactor auth",
  url: null,
  runs: 2,
  findings: 9,
  findingsFound: 11,
  medianDurationMs: 120_000,
  lastRunAt: 0,
  strip: [
    { runId: 1, tokens: 0, outcome: "failed" },
    { runId: 2, tokens: 2_310_000, outcome: "posted" },
  ],
};
const run = (id: number, overrides: Partial<UsageRun>): UsageRun => ({
  ...tokens(1_000_000),
  id,
  outcome: "posted",
  status: "posted",
  headSha: "a1b2c3d4e5f6",
  startedAt: 1_760_000_000_000,
  durationMs: 190_000,
  findings: 3,
  questions: 0,
  findingsFound: 3,
  questionsFound: 0,
  models: ["claude-opus"],
  accounts: ["work@example.com"],
  reusedChunks: 0,
  chunks: 4,
  usageReported: true,
  ...overrides,
});

let host: HTMLDivElement;
let root: Root;
let client: QueryClient;
let onUrlUpdate: ReturnType<typeof vi.fn<OnUrlUpdateFunction>>;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.clearAllMocks();
  vi.mocked(setupApi.usageReport).mockResolvedValue(report());
  vi.mocked(setupApi.usagePrs).mockResolvedValue({ items: [pr], total: 1, nextOffset: null });
  vi.mocked(setupApi.usagePrRuns).mockResolvedValue([
    run(2, {}),
    run(1, { outcome: "failed", status: "failed", usageReported: false, tokens: 0, findings: 0 }),
  ]);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  onUrlUpdate = vi.fn<OnUrlUpdateFunction>();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  host.remove();
  vi.unstubAllGlobals();
});

async function render(searchParams = "", hasMemory = false) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <NuqsTestingAdapter searchParams={searchParams} onUrlUpdate={onUrlUpdate} hasMemory={hasMemory}>
          <UsagePage />
        </NuqsTestingAdapter>
      </QueryClientProvider>,
    ),
  );
  await settle();
}
async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 80));
  });
}
function button(text: string, scope: ParentNode = host) {
  const found = [...scope.querySelectorAll("button")].find(
    (element) => element.getAttribute("aria-label") === text || element.textContent?.trim() === text,
  );
  if (!found) throw new Error(`Button not found: ${text}`);
  return found;
}
async function click(element: HTMLElement) {
  await act(async () => element.click());
  await settle();
}

describe("Usage page", () => {
  it("leads with the period's tokens and the totals", async () => {
    await render();
    expect(host.querySelector("[data-usage-hero]")?.textContent).toBe("12.42M");
    expect(host.textContent).toContain("tokens · 142 runs on 58 PRs");
    expect(host.textContent).toContain("2 runs without reported usage");
    expect(host.textContent).toContain("Read from cache");
    expect(host.textContent).toContain("+ 24.84M");
    // Findings lead with what the reviews kept; posted is the published part.
    expect(host.textContent).toContain("350");
    expect(host.textContent).toContain("311 posted · 44 questions");
    expect(host.textContent).toContain("4m 12s");
    expect(host.textContent).toContain("38%");
    // 4M of 12.42M is above the 25% overhead threshold.
    expect([...host.querySelectorAll("dd")].find((dd) => dd.textContent === "4M")?.className).toContain("text-warning");
    // One known provider: no provider list.
    expect(host.querySelector('[aria-label="Tokens by provider"]')).toBeNull();
    expect(setupApi.usageReport).toHaveBeenCalledWith({ days: 30, repoId: null, tz: -new Date().getTimezoneOffset() });
  });

  it("writes the range and the repository filter to the URL", async () => {
    await render("", true);
    const ninety = [...host.querySelectorAll<HTMLInputElement>('input[type="radio"]')].find(
      (input) => input.parentElement?.textContent === "90d",
    );
    await click(ninety!);
    expect(onUrlUpdate.mock.lastCall?.[0].searchParams.get("days")).toBe("90");
    expect(onUrlUpdate.mock.lastCall?.[0].options.history).toBe("replace");
    expect(setupApi.usageReport).toHaveBeenLastCalledWith(expect.objectContaining({ days: 90 }));

    await click(button("Filter by repository"));
    const items = [...document.querySelectorAll('[role="menuitemradio"]')];
    expect(items.map((item) => item.getAttribute("aria-checked"))).toEqual(["true", "false", "false"]);
    await click(button("acme/web", document.body));
    expect(onUrlUpdate.mock.lastCall?.[0].searchParams.get("repo")).toBe("2");
    expect(setupApi.usageReport).toHaveBeenLastCalledWith(expect.objectContaining({ days: 90, repoId: 2 }));
  });

  it("switches breakdown tabs and labels unattributed usage", async () => {
    await render();
    const tabs = [...host.querySelectorAll('[role="tab"]')].map((tab) => tab.textContent);
    // A single known account does not earn an Accounts tab.
    expect(tabs).toEqual(["Merge requests", "Repositories", "Models"]);
    await click(button("Models"));
    expect(button("Models").getAttribute("aria-selected")).toBe("true");
    const panel = host.querySelector('[role="tabpanel"]')!;
    expect(panel.textContent).toContain("claude-opus");
    expect(panel.textContent).toContain("Unattributed");
    const hint = panel.querySelector<HTMLButtonElement>('button[aria-label*="before OpenCodeReview tracked usage"]');
    expect(hint).not.toBeNull();
    // The explanation shows on focus, not only on a slow native hover title.
    await act(async () => hint?.focus());
    expect(document.body.querySelector('[role="tooltip"]')?.textContent).toContain(
      "before OpenCodeReview tracked usage",
    );
    await act(async () => hint?.blur());
    expect(document.body.querySelector('[role="tooltip"]')).toBeNull();
  });

  it("shows the Accounts tab once two accounts used tokens", async () => {
    vi.mocked(setupApi.usageReport).mockResolvedValue(
      report({ byAccount: [group("7", "work@example.com", 9_000_000), group("8", "home@example.com", 3_420_000)] }),
    );
    await render();
    await click(button("Accounts"));
    expect(host.querySelector('[role="tabpanel"]')?.textContent).toContain("home@example.com");
  });

  it("loads an MR's runs only when it is expanded, once, and dims runs without usage", async () => {
    await render();
    expect(setupApi.usagePrRuns).not.toHaveBeenCalled();
    const row = host.querySelector<HTMLButtonElement>('[role="tabpanel"] button[aria-expanded]')!;
    expect(row.textContent).toContain("Refactor auth");
    expect(row.getAttribute("aria-expanded")).toBe("false");
    expect(row.querySelector('[role="img"]')?.getAttribute("aria-label")).toBe("2 runs: 1 falhou, 1 publicado");
    await click(row);
    expect(row.getAttribute("aria-expanded")).toBe("true");
    expect(setupApi.usagePrRuns).toHaveBeenCalledTimes(1);
    expect(setupApi.usagePrRuns).toHaveBeenCalledWith(412, { days: 30, tz: -new Date().getTimezoneOffset() });
    const panel = host.querySelector(`#${row.getAttribute("aria-controls")}`)!;
    expect(panel.textContent).toContain("2 runs in this period · 1 failed");
    expect(panel.querySelector('a[href="/platform/reviews?pr=412"]')?.textContent).toBe("Open review →");
    const dimmed = panel.querySelector('li[data-usage-reported="false"]')!;
    expect(dimmed.className).toContain("opacity-50");
    expect(dimmed.textContent).toContain("—");
    expect(dimmed.textContent).toContain("a1b2c3d4");
    expect(panel.querySelector('li[data-usage-reported="true"]')?.textContent).toContain("1M");

    await click(row);
    expect(row.getAttribute("aria-expanded")).toBe("false");
    expect(host.querySelector("li[data-usage-reported]")).toBeNull();
    await click(row);
    expect(setupApi.usagePrRuns).toHaveBeenCalledTimes(1);
  });

  it("says what fills an empty period and offers to widen it", async () => {
    vi.mocked(setupApi.usageReport).mockResolvedValue(
      report({ totals: { ...report().totals, runs: 0, prs: 0, tokens: 0 } }),
    );
    await render("?days=7&repo=1");
    expect(host.textContent).toContain("Usage appears after a review runs. Start one from Reviews.");
    expect(host.querySelector('a[href="/platform/reviews"]')).not.toBeNull();
    expect(setupApi.usagePrs).not.toHaveBeenCalled();
    await click(button("Clear filter"));
    expect(onUrlUpdate.mock.lastCall?.[0].searchParams.has("repo")).toBe(false);
    await click(button("Show 90 days"));
    expect(onUrlUpdate.mock.lastCall?.[0].searchParams.get("days")).toBe("90");
  });

  it("shares an image built from totals only, without names", async () => {
    const data = JSON.stringify(shareCardData(report()));
    for (const name of ["acme", "work@example.com", "claude-opus", "Refactor auth"]) expect(data).not.toContain(name);
    expect(shareCardData(report()).tokens).toBe("12.42M");
    await render();
    await click(button("Share"));
    const dialog = document.body.querySelector('[role="dialog"][aria-label="Share usage image"]');
    expect(dialog?.textContent).toContain("no repository, merge request, account or model names");
    await click(button("Close", dialog ?? document.body));
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
  });

  it("keeps the shared image while the report refetches", async () => {
    const revoke = vi.fn();
    vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: () => "blob:usage", revokeObjectURL: revoke }));
    await render();
    await click(button("Share"));
    expect(renderShareCard).toHaveBeenCalledTimes(1);
    const tz = -new Date().getTimezoneOffset();
    await act(async () => client.setQueryData(["usage", "report", 30, null, tz], report({ hasRunning: true })));
    await settle();
    expect(renderShareCard).toHaveBeenCalledTimes(1);
    expect(revoke).not.toHaveBeenCalled();
    expect(document.body.querySelector('[role="dialog"] img')?.getAttribute("src")).toBe("blob:usage");
  });

  it("points an old server at a Docker rebuild", async () => {
    vi.mocked(setupApi.usageReport).mockRejectedValue(new Error("route not found"));
    await render();
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("docker compose up --build server");
  });
});
