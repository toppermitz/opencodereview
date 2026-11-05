import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { AiAccount, ReviewEngine } from "../../types";
import { EnginePicker } from "./EnginePicker";

vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listAccounts: vi.fn(),
    reviewEngines: vi.fn(),
    saveReviewEngine: vi.fn(),
    reorderAccounts: vi.fn(),
    updateAccount: vi.fn(),
    accountUsage: vi.fn(),
  },
}));

const account = (id: number, provider: AiAccount["provider"], label: string): AiAccount => ({
  id,
  provider,
  label,
  priority: id,
  enabled: true,
  status: "ok",
  limitedUntil: null,
  lastError: null,
  lastUsedAt: null,
  createdAt: 0,
  signedIn: true,
});
const engines: ReviewEngine[] = [
  {
    provider: "claude",
    name: "Claude",
    model: "claude-opus-5-5",
    effort: "medium",
    label: "Claude · Opus 5.5 · medium",
    models: [
      { id: "claude-opus-5-5", label: "Opus 5.5", efforts: ["low", "medium", "high"], defaultEffort: "medium" },
      { id: "claude-sonnet-5-5", label: "Sonnet 5.5", efforts: ["low", "medium", "high"], defaultEffort: "medium" },
    ],
  },
  {
    provider: "codex",
    name: "ChatGPT",
    model: "default",
    effort: "medium",
    label: "ChatGPT · Codex default · medium",
    models: [{ id: "default", label: "Codex default", efforts: ["low", "medium"], defaultEffort: "medium" }],
  },
];

beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("shows the next review's engine and changes model, effort and provider", async () => {
  vi.mocked(setupApi.listAccounts).mockResolvedValue([account(1, "claude", "Claude"), account(2, "codex", "me@x.com")]);
  vi.mocked(setupApi.reviewEngines).mockResolvedValue(engines);
  vi.mocked(setupApi.saveReviewEngine).mockResolvedValue(engines);
  vi.mocked(setupApi.accountUsage).mockResolvedValue([]);
  vi.mocked(setupApi.reorderAccounts).mockResolvedValue([
    account(2, "codex", "me@x.com"),
    account(1, "claude", "Claude"),
  ]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const button = (text: string) =>
    [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(text))!;
  try {
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <EnginePicker />
        </QueryClientProvider>,
      ),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(container.textContent).toContain("Opus 5.5");
    // Effort and account live in the trigger's label, not its text.
    expect(container.querySelector("button[aria-haspopup]")?.getAttribute("aria-label")).toBe(
      "Opus 5.5 · Medium · Claude",
    );
    await act(async () => container.querySelector<HTMLButtonElement>("button[aria-haspopup]")!.click());
    // The rail holds the providers; the menu shows the next account's provider first.
    expect(container.querySelector('button[aria-label="ChatGPT"]')).not.toBeNull();
    expect(container.textContent).toContain("default");
    await act(async () => button("Sonnet 5.5").click());
    expect(setupApi.saveReviewEngine).toHaveBeenCalledWith({
      provider: "claude",
      model: "claude-sonnet-5-5",
      effort: "medium",
    });
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent === "High")!.click(),
    );
    expect(setupApi.saveReviewEngine).toHaveBeenLastCalledWith({
      provider: "claude",
      model: "claude-opus-5-5",
      effort: "high",
    });
    // Showing ChatGPT changes nothing until one of its accounts is chosen.
    await act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="ChatGPT"]')!.click());
    expect(setupApi.reorderAccounts).not.toHaveBeenCalled();
    expect(container.textContent).toContain("New reviews run on Claude · Claude");
    await act(async () => button("me@x.com").click());
    expect(setupApi.reorderAccounts).toHaveBeenCalledWith([2, 1]);
  } finally {
    await act(async () => root.unmount());
    container.remove();
    client.clear();
  }
});

it("switches to another account of the provider, resuming a paused one and skipping one at its limit", async () => {
  const paused = { ...account(2, "claude", "Brito"), enabled: false };
  // Paused and still at its limit: resuming it would not start reviews on it, so it can't be picked.
  const limited = {
    ...account(3, "claude", "Bruna"),
    enabled: false,
    status: "limited" as const,
    limitedUntil: Date.now() + 3_600_000,
  };
  vi.mocked(setupApi.listAccounts).mockResolvedValue([account(1, "claude", "Bruno"), paused, limited]);
  vi.mocked(setupApi.reviewEngines).mockResolvedValue(engines);
  vi.mocked(setupApi.accountUsage).mockResolvedValue([
    { accountId: 1, windows: [{ windowMins: 300, usedPercent: 97, resetsAt: null }] },
    { accountId: 2, windows: [{ windowMins: 300, usedPercent: 0, resetsAt: null }] },
  ]);
  vi.mocked(setupApi.updateAccount).mockResolvedValue([]);
  vi.mocked(setupApi.reorderAccounts).mockResolvedValue([
    { ...paused, enabled: true },
    account(1, "claude", "Bruno"),
    limited,
  ]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const radio = (text: string) =>
    [...container.querySelectorAll<HTMLButtonElement>('[aria-label="Account"] [role="radio"]')].find((item) =>
      item.textContent?.includes(text),
    )!;
  try {
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <EnginePicker />
        </QueryClientProvider>,
      ),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => container.querySelector<HTMLButtonElement>("button[aria-haspopup]")!.click());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(radio("Bruno").getAttribute("aria-checked")).toBe("true");
    expect(radio("Bruno").textContent).toContain("3% restante");
    expect(radio("Brito").textContent).toContain("Pausada · 100% restante");
    expect(radio("Bruna").disabled).toBe(true);
    expect(radio("Bruna").textContent).toContain("Limite atingido · reinicia em 1 h");
    expect(radio("Bruna").title).toContain("Usage limit reached");
    await act(async () => radio("Brito").click());
    expect(setupApi.updateAccount).toHaveBeenCalledWith(2, { enabled: true });
    expect(setupApi.reorderAccounts).toHaveBeenCalledWith([2, 1, 3]);

    await act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="Add an account"]')!.click());
    expect(container.textContent).toContain("Add an account");
    expect(container.textContent).toContain("ChatGPT");
  } finally {
    await act(async () => root.unmount());
    container.remove();
    client.clear();
  }
});

it("says when a used-up account resets, before any review has hit its limit", async () => {
  vi.mocked(setupApi.listAccounts).mockResolvedValue([account(1, "claude", "Bruno"), account(2, "claude", "Brisa")]);
  vi.mocked(setupApi.reviewEngines).mockResolvedValue(engines);
  vi.mocked(setupApi.accountUsage).mockResolvedValue([
    { accountId: 1, windows: [{ windowMins: 300, usedPercent: 100, resetsAt: null }] },
    {
      accountId: 2,
      windows: [
        { windowMins: 300, usedPercent: 100, resetsAt: new Date(Date.now() + 3_600_000).toISOString() },
        { windowMins: 10_080, usedPercent: 100, resetsAt: Date.now() + 7_200_000 },
      ],
    },
  ]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const radio = (text: string) =>
    [...container.querySelectorAll<HTMLButtonElement>('[aria-label="Account"] [role="radio"]')].find((item) =>
      item.textContent?.includes(text),
    )!;
  try {
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <EnginePicker />
        </QueryClientProvider>,
      ),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    await act(async () => container.querySelector<HTMLButtonElement>("button[aria-haspopup]")!.click());
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    // Without a reset time there is nothing better to say than how much is left.
    expect(radio("Bruno").textContent).toContain("0% restante");
    // Both windows used up: the account is back only when the later one resets.
    expect(radio("Brisa").textContent).toContain("Reinicia em 2 h");
  } finally {
    await act(async () => root.unmount());
    container.remove();
    client.clear();
  }
});
