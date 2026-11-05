import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { AiAccount, ReviewEngine } from "../../types";
import { ProvidersPanel } from "./ProvidersPanel";

vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listAccounts: vi.fn(),
    accountUsage: vi.fn(() => Promise.resolve([])),
    reviewEngines: vi.fn(),
    saveReviewEngine: vi.fn(),
    reorderAccounts: vi.fn(),
    updateAccount: vi.fn(),
    deleteAccount: vi.fn(),
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
const engine = (provider: AiAccount["provider"], name: string): ReviewEngine => ({
  provider,
  name,
  model: "m1",
  effort: "medium",
  label: `${name} · Model 1 · medium`,
  models: [
    { id: "m1", label: "Model 1", efforts: ["low", "medium", "high"], defaultEffort: "medium" },
    { id: "m2", label: "Model 2", efforts: ["low", "medium"], defaultEffort: "low" },
  ],
});

beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("lists connected providers in review order and opens one to set its model, effort and accounts", async () => {
  const accounts = [account(1, "codex", "me@x.com"), account(2, "claude", "Work"), account(3, "codex", "alt@x.com")];
  const engines = [engine("claude", "Claude"), engine("codex", "ChatGPT")];
  vi.mocked(setupApi.listAccounts).mockResolvedValue(accounts);
  vi.mocked(setupApi.reviewEngines).mockResolvedValue(engines);
  vi.mocked(setupApi.saveReviewEngine).mockResolvedValue(engines);
  vi.mocked(setupApi.reorderAccounts).mockResolvedValue(accounts);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const settle = () =>
    act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  const button = (text: string) =>
    [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.includes(text))!;
  try {
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <ProvidersPanel />
        </QueryClientProvider>,
      ),
    );
    await settle();
    const rows = [...container.querySelectorAll("button")].map((item) => item.textContent);
    expect(rows.findIndex((text) => text?.includes("ChatGPT"))).toBeLessThan(
      rows.findIndex((text) => text?.includes("Claude")),
    );
    expect(button("ChatGPT").textContent).toContain("Próxima revisão");
    expect(button("ChatGPT").textContent).toContain("2 accounts · Model 1 · medium");

    await act(async () => button("Claude").click());
    await settle();
    expect(container.textContent).toContain("Work");
    expect(container.textContent).not.toContain("me@x.com");
    await act(async () => button("high").click());
    expect(setupApi.saveReviewEngine).toHaveBeenCalledWith({ provider: "claude", model: "m1", effort: "high" });
    await act(async () => button("Use for new reviews").click());
    expect(setupApi.reorderAccounts).toHaveBeenCalledWith([2, 1, 3]);
    await act(async () => button("Provedores").click());
    expect(container.textContent).toContain("Conectado");
  } finally {
    await act(async () => root.unmount());
    container.remove();
    client.clear();
  }
});
