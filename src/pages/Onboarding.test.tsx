import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type SetupStatus, setupApi } from "../services/setupApi";
import type { AiProvider, Repo, ReviewCard } from "../types";
import { Onboarding } from "./Onboarding";

const navigate = vi.fn();
vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, ...props }: ComponentProps<"a"> & { to: string }) => <a href={to} {...props} />,
  useNavigate: () => navigate,
}));

vi.mock("../services/setupApi", () => ({
  setupApi: {
    setupStatus: vi.fn(),
    aiProviders: vi.fn(),
    gitlabAuthEvents: vi.fn(),
    gitlabAuthStart: vi.fn(),
    claudeAuthStart: vi.fn(),
    claudeAuthEvents: vi.fn(),
    claudeAuthInput: vi.fn(),
    claudeAuthCancel: vi.fn(),
    claudeTokenSave: vi.fn(),
    codexAuthStart: vi.fn(),
    codexAuthEvents: vi.fn(),
    codexAuthCancel: vi.fn(),
    refreshWorkspaces: vi.fn(),
    listWorkspaceRepos: vi.fn(),
    saveSelectedWorkspaces: vi.fn(),
    saveRepoSubscription: vi.fn(),
    pollReviews: vi.fn(),
    listReviewBoard: vi.fn(),
    startReview: vi.fn(),
  },
}));

const workspace = {
  id: 1,
  login: "acme",
  kind: "org",
  selected: false,
  repoCount: 2,
  reviewSkillRepoCount: 0,
  refreshedAt: null,
  monitorAll: false,
};
const repo = (id: number, name: string, change: Partial<Repo> = {}): Repo => ({
  id,
  workspaceId: 1,
  owner: "acme",
  name,
  fullName: `acme/${name}`,
  description: null,
  isPrivate: true,
  defaultBranch: "main",
  skillPath: null,
  skillStatus: "unknown",
  skillCheckedAt: null,
  skillError: null,
  subscriptionEnabled: false,
  reviewMode: "manual",
  skipDraftPrs: true,
  skipSelfPrs: true,
  pollError: null,
  pollErrorUrl: null,
  pollErrorAt: null,
  ...change,
});
const github = { loggedIn: true, login: "octo", host: "gitlab.example.com", detail: "Connected" };
const status = (change: Partial<SetupStatus>): SetupStatus => ({
  auth: { loggedIn: false, login: null, host: "gitlab.example.com", detail: "Not connected" },
  workspaces: [],
  selectedWorkspaces: [],
  selectedRepos: [],
  complete: false,
  nextStep: 1,
  ...change,
});
const provider = (change: Partial<AiProvider>): AiProvider => ({
  provider: "claude",
  name: "Claude",
  installed: true,
  version: "2.1.292",
  accounts: 0,
  ready: 0,
  label: null,
  ...change,
});
const providers = (claude: Partial<AiProvider> = {}, codex: Partial<AiProvider> = {}) => [
  provider(claude),
  provider({ provider: "codex", name: "ChatGPT", version: "0.163.0", ...codex }),
];

let client: QueryClient;
let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.clearAllMocks();
  vi.mocked(setupApi.gitlabAuthEvents).mockResolvedValue({ running: false, events: [] });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  client.clear();
  vi.unstubAllGlobals();
});

const settle = () =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 10));
  });
const button = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.trim() === text);

async function render(setup: SetupStatus) {
  vi.mocked(setupApi.setupStatus).mockResolvedValue(setup);
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <Onboarding />
      </QueryClientProvider>,
    ),
  );
  await settle();
}

describe("onboarding", () => {
  it("starts with GitHub and keeps Continue disabled until it is connected", async () => {
    await render(status({}));
    expect(container.querySelector("h1")?.textContent).toBe("Connect GitLab");
    expect(container.querySelector('[aria-label="Step 1 of 4"] [aria-current="step"]')).not.toBeNull();
    expect(container.querySelector('[aria-label="GitLab"]')?.textContent).toContain("Not connected");
    expect(button("Continue")?.disabled).toBe(true);
    await act(async () => button("Connect")!.click());
    expect(setupApi.gitlabAuthStart).toHaveBeenCalled();
  });

  it("explains an unreachable server instead of showing a raw fetch error", async () => {
    vi.mocked(setupApi.setupStatus).mockRejectedValue(new TypeError("Failed to fetch"));
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <Onboarding />
        </QueryClientProvider>,
      ),
    );
    await vi.waitFor(
      () => expect(container.querySelector("h1")?.textContent).toBe("OpenCodeReview can't reach its server"),
      { timeout: 3000 },
    );
    expect(container.textContent).toContain("docker compose up -d");
    expect(container.textContent).not.toContain("Failed to fetch");
  });

  it("lists Claude and ChatGPT with their CLI versions and connection state", async () => {
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers({}, { installed: false, version: null }));
    await render(status({ auth: github, nextStep: 2 }));
    expect(container.querySelector("h1")?.textContent).toBe("Connect your plan");
    await vi.waitFor(() => expect(container.querySelector('[aria-label="Claude"]')).not.toBeNull());
    const claude = container.querySelector('[aria-label="Claude"]')!;
    expect(claude.textContent).toContain("Pro, Max or Team plan");
    expect(claude.textContent).not.toContain("v2.1.292");
    expect(claude.textContent).not.toContain("Not connected");
    expect(claude.querySelector("button")?.textContent).toBe("Connect");
    const chatgpt = container.querySelector('[aria-label="ChatGPT"]')!;
    expect(chatgpt.textContent).toContain("Not installed");
    expect(chatgpt.querySelector("button")).toBeNull();
    expect(button("Continue")?.disabled).toBe(true);
    expect(container.textContent).not.toContain("Connect an AI provider to continue.");
  });

  it("connects ChatGPT in the browser, or with a one-time code from the row menu", async () => {
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers());
    vi.mocked(setupApi.codexAuthStart).mockResolvedValue(undefined);
    vi.mocked(setupApi.codexAuthCancel).mockResolvedValue({ running: false });
    vi.mocked(setupApi.codexAuthEvents).mockResolvedValue({
      running: true,
      events: [{ kind: "prompt", url: "https://auth.openai.com/oauth/authorize?state=s" }],
    });
    await render(status({ auth: github, nextStep: 2 }));
    const row = () => container.querySelector('[aria-label="ChatGPT"]')!;
    await act(async () => row().querySelector<HTMLButtonElement>("button")!.click());
    await settle();
    expect(setupApi.codexAuthStart).toHaveBeenCalledWith(undefined, "browser");
    await vi.waitFor(() =>
      expect(row().querySelector("a")?.getAttribute("href")).toBe("https://auth.openai.com/oauth/authorize?state=s"),
    );
    await act(async () => button("Cancel")!.click());
    await settle();

    vi.mocked(setupApi.codexAuthEvents).mockResolvedValue({
      running: true,
      events: [{ kind: "prompt", url: "https://auth.openai.com/codex/device", code: "7KQ2-9XWPL" }],
    });
    await act(async () => row().querySelector<HTMLButtonElement>('button[aria-label="More ChatGPT options"]')!.click());
    await act(async () =>
      [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')]
        .find((item) => item.textContent?.startsWith("Sign in with a code"))!
        .click(),
    );
    await settle();
    expect(setupApi.codexAuthStart).toHaveBeenLastCalledWith(undefined, "device");
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Codex sign-in code"]')?.textContent).toBe("7KQ2-9XWPL"),
    );
  });

  it("closes the Claude sign-in with one Cancel, even when the server fails to cancel", async () => {
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers());
    vi.mocked(setupApi.claudeAuthStart).mockResolvedValue(undefined);
    vi.mocked(setupApi.claudeAuthCancel).mockRejectedValue(new Error("no sign-in running"));
    vi.mocked(setupApi.claudeAuthEvents).mockResolvedValue({
      running: true,
      events: [{ kind: "prompt", url: "https://claude.ai/oauth/authorize?state=s" }],
    });
    await render(status({ auth: github, nextStep: 2 }));
    const row = () => container.querySelector('[aria-label="Claude"]')!;
    await act(async () => row().querySelector<HTMLButtonElement>("button")!.click());
    await vi.waitFor(() =>
      expect(row().querySelector("a")?.getAttribute("href")).toBe("https://claude.ai/oauth/authorize?state=s"),
    );
    await act(async () => button("Cancel")!.click());
    await settle();
    expect(setupApi.claudeAuthCancel).toHaveBeenCalled();
    expect(button("Cancel")).toBeUndefined();
    expect(row().querySelector("button")?.textContent).toBe("Connect");
  });

  it("lets Claude sign in with a pasted setup token", async () => {
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers());
    vi.mocked(setupApi.claudeTokenSave).mockResolvedValue({ ok: true });
    await render(status({ auth: github, nextStep: 2 }));
    await act(async () =>
      container.querySelector<HTMLButtonElement>('button[aria-label="More Claude options"]')!.click(),
    );
    await act(async () =>
      [...document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')]
        .find((item) => item.textContent?.startsWith("Paste a token"))!
        .click(),
    );
    const input = container.querySelector<HTMLInputElement>('input[aria-label="Claude token"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "sk-ant-oat01-fixture");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers({ accounts: 1, ready: 1, label: "Claude" }));
    vi.mocked(setupApi.setupStatus).mockResolvedValue(status({ auth: github, nextStep: 3 }));
    await act(async () => button("Save token")!.click());
    await settle();
    expect(setupApi.claudeTokenSave).toHaveBeenCalledWith("sk-ant-oat01-fixture", undefined);
    await vi.waitFor(() =>
      expect(container.querySelector('[aria-label="Claude"]')?.textContent).toContain("Connected"),
    );
    expect(container.querySelector("h1")?.textContent).toBe("Connect your plan");
    expect(button("Continue")?.disabled).toBe(false);
  });

  it("picks repositories across organizations in one list and saves them", async () => {
    vi.mocked(setupApi.refreshWorkspaces).mockResolvedValue([workspace]);
    vi.mocked(setupApi.listWorkspaceRepos).mockResolvedValue([
      repo(1, "app", { subscriptionEnabled: true }),
      repo(2, "api"),
    ]);
    vi.mocked(setupApi.saveSelectedWorkspaces).mockResolvedValue([{ ...workspace, selected: true }]);
    vi.mocked(setupApi.saveRepoSubscription).mockImplementation(async (value) => value);
    vi.mocked(setupApi.aiProviders).mockResolvedValue(providers({ ready: 1, accounts: 1, label: "Claude" }));
    vi.mocked(setupApi.pollReviews).mockResolvedValue({
      pollIntervalSeconds: 300,
      running: true,
      lastPollAt: null,
      nextPollAt: null,
      lastError: null,
      monitoredRepoCount: 2,
    });
    vi.mocked(setupApi.listReviewBoard).mockResolvedValue([]);
    await render(status({ auth: github, nextStep: 3 }));
    expect(container.querySelector("h1")?.textContent).toBe("Pick your repositories");
    await vi.waitFor(() => expect(container.querySelectorAll('[role="checkbox"]')).toHaveLength(2));
    expect(container.querySelector('[role="checkbox"][aria-checked="true"]')?.textContent).toContain("acme/app");
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>('[role="checkbox"]')]
        .find((item) => item.textContent?.includes("acme/api"))!
        .click(),
    );
    await act(async () => button("Continue with 2 repositories")!.click());
    await settle();
    expect(setupApi.saveSelectedWorkspaces).toHaveBeenCalledWith(["acme"]);
    expect(setupApi.saveRepoSubscription).toHaveBeenCalledTimes(1);
    expect(setupApi.saveRepoSubscription).toHaveBeenCalledWith(expect.objectContaining({ id: 2 }), true);
    expect(container.querySelector("h1")?.textContent).toBe("You're all set");
    expect(container.textContent).toContain("2 selected");
  });

  it("confirms the setup and starts the first review on an open merge request", async () => {
    const pr = {
      id: 9,
      repoFullName: "acme/app",
      number: 42,
      title: "Add billing",
      author: "ana",
      state: "open",
      lane: "needs_review",
      updatedAt: 1,
    } as ReviewCard;
    vi.mocked(setupApi.aiProviders).mockResolvedValue(
      providers({}, { ready: 1, accounts: 1, label: "dev@example.com" }),
    );
    vi.mocked(setupApi.pollReviews).mockResolvedValue({
      pollIntervalSeconds: 300,
      running: false,
      lastPollAt: 1,
      nextPollAt: null,
      lastError: null,
      monitoredRepoCount: 1,
    });
    vi.mocked(setupApi.listReviewBoard).mockResolvedValue([
      {
        lane: "needs_review",
        title: "Needs review",
        items: [
          pr,
          { ...pr, id: 10, state: "merged", title: "Old" },
          { ...pr, id: 11, lane: "reviewed_cleanly", title: "Done already" },
        ],
      },
    ]);
    vi.mocked(setupApi.startReview).mockResolvedValue(undefined);
    await render(status({ auth: github, selectedRepos: [repo(1, "app")], complete: true, nextStep: 4 }));
    expect(container.querySelector("h1")?.textContent).toBe("You're all set");
    expect(container.textContent).toContain("ChatGPT · dev@example.com");
    expect(setupApi.pollReviews).toHaveBeenCalled();
    await vi.waitFor(() => expect(container.querySelector('[aria-label="Add billing"]')).not.toBeNull());
    expect(container.querySelector('[aria-label="Old"]')).toBeNull();
    expect(container.querySelector('[aria-label="Done already"]')).toBeNull();
    await act(async () => button("Review")!.click());
    await settle();
    expect(setupApi.startReview).toHaveBeenCalledWith(9);
    expect(navigate).toHaveBeenCalledWith({ to: "/platform/reviews" });
  });
});
