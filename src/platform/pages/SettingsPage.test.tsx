import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { setupApi } from "../../services/setupApi";
import type { Settings } from "../../types";
import { SettingsPage } from "./SettingsPage";

const navigateTo = vi.fn();
vi.mock("@tanstack/react-router", async (original) => ({
  ...(await original<typeof import("@tanstack/react-router")>()),
  useBlocker: vi.fn(),
  useNavigate: () => navigateTo,
}));
vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listAccounts: vi.fn(),
    accountUsage: vi.fn(() => Promise.resolve([])),
    updateAccount: vi.fn(),
    reorderAccounts: vi.fn(),
    deleteAccount: vi.fn(),
    setupStatus: vi.fn(),
    claudeAuthStatus: vi.fn(),
    gitlabAuthLogout: vi.fn(),
    saveSettingsBatch: vi.fn(),
  },
}));

const dialogMethods = ["showModal", "close"].map(
  (name) => [name, Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, name)] as const,
);
beforeAll(() => {
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
afterAll(() => {
  for (const [name, descriptor] of dialogMethods) {
    if (descriptor) Object.defineProperty(HTMLDialogElement.prototype, name, descriptor);
    else Reflect.deleteProperty(HTMLDialogElement.prototype, name);
  }
});

const repos = [
  {
    id: 1,
    owner: "example",
    fullName: "example/repository-with-a-long-name-that-needs-room",
    defaultBranch: "main",
    isPrivate: true,
    subscriptionEnabled: true,
  },
  {
    id: 2,
    owner: "example",
    fullName: "example/another-repository",
    defaultBranch: "develop",
    isPrivate: false,
    subscriptionEnabled: false,
  },
];
const workspace = {
  id: 1,
  login: "example",
  repoCount: 2,
  reviewSkillRepoCount: 0,
  refreshedAt: null,
  kind: "org",
  selected: true,
  monitorAll: false,
};
let client: QueryClient;
let container: HTMLDivElement;
let root: Root;

beforeEach(async () => {
  vi.clearAllMocks();
  vi.mocked(setupApi.setupStatus).mockReset();
  vi.mocked(setupApi.listAccounts).mockResolvedValue([]);
  vi.mocked(setupApi.claudeAuthStatus).mockReset();
  vi.mocked(setupApi.saveSettingsBatch)
    .mockReset()
    .mockImplementation(async (payload) => {
      const settings = client.getQueryData<Settings>(["setup", "settings", "review"])!;
      const saved = {
        ...settings,
        ...Object.fromEntries(
          Object.entries(payload.settings).map(([key, value]) => {
            const field = key.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()) as keyof Settings;
            return [field, typeof settings[field] === "string" ? value : JSON.parse(value)];
          }),
        ),
      };
      return {
        settings: saved,
        workspaces: [{ ...workspace, id: 1, selected: payload.repositories?.logins.includes("example") ?? true }],
        selectedRepos: repos
          .filter((repo) =>
            payload.repositories ? payload.repositories.repoIds.includes(repo.id) : repo.subscriptionEnabled,
          )
          .map((repo) => ({ ...repo, subscriptionEnabled: true })) as Awaited<
          ReturnType<typeof setupApi.saveSettingsBatch>
        >["selectedRepos"],
      };
    });
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  client.setQueryData(["setup", "status"], {
    auth: { loggedIn: true, detail: "", login: "developer", host: "gitlab.example.com" },
    workspaces: [workspace],
    selectedWorkspaces: [workspace],
    selectedRepos: [repos[0]],
  });
  client.setQueryData(["setup", "settings", "repos", "example"], repos);
  client.setQueryData(["setup", "settings", "claude-auth"], { loggedIn: true, message: "Connected" });
  client.setQueryData(["setup", "settings", "review"], {
    autoSkipKeywords: ["deps"],
    reviewAssignedOnly: false,
    pollIntervalSeconds: 60,
    reviewAutoEnabled: false,
    reviewConcurrency: 3,
    reviewDepthMode: "auto",
    reviewDeepThreshold: 10,
    reviewChunkSize: 5,
    reviewParallelChunks: 2,
    reviewMaxFiles: 100,
    reviewSuggestionsEnabled: false,
    prSummaryCommentEnabled: false,
    reviewResultMode: "silent",
    autoPostMode: "off",
    dismissStaleApprovals: false,
    reviewArchiveRetentionDays: 1,
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <SettingsPage />
      </QueryClientProvider>,
    ),
  );
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  client.clear();
  vi.unstubAllGlobals();
});

function button(label: string) {
  const element = Array.from(container.querySelectorAll("button")).find((item) => item.textContent === label);
  expect(element, label).toBeDefined();
  return element!;
}

async function navigate(label: string) {
  const element = Array.from(container.querySelectorAll("nav[aria-label='Seções de configurações'] button")).find(
    (item) => item.textContent?.includes(label),
  );
  expect(element, label).toBeDefined();
  await act(async () => (element as HTMLButtonElement).click());
}

async function inputValue(input: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("settings workspace", () => {
  it.each([["GitLab", "gitlabAuthLogout"]] as const)(
    "signs out of %s only after confirmation and returns to onboarding",
    async (title, method) => {
      vi.mocked(setupApi.setupStatus).mockResolvedValue(
        client.getQueryData<Awaited<ReturnType<typeof setupApi.setupStatus>>>(["setup", "status"])!,
      );
      vi.mocked(setupApi.claudeAuthStatus).mockResolvedValue({ loggedIn: true, message: "Connected" });
      vi.mocked(setupApi[method]).mockResolvedValue({ ok: true });
      const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
      await navigate("Integrações");
      const signOut = () =>
        Array.from(
          Array.from(container.querySelectorAll("#settings-integrations .settings-row"))
            .find((item) => item.querySelector("h3")?.textContent === title)!
            .querySelectorAll("button"),
        ).find((item) => item.textContent === "Sair")!;
      await act(async () => signOut().click());
      expect(setupApi[method]).not.toHaveBeenCalled();
      await act(async () => {
        signOut().click();
        await new Promise((resolve) => setTimeout(resolve, 0));
      });
      expect(confirm).toHaveBeenCalledTimes(2);
      expect(setupApi[method]).toHaveBeenCalledTimes(1);
      expect(navigateTo).toHaveBeenCalledWith({ to: "/" });
      expect(setupApi.saveSettingsBatch).not.toHaveBeenCalled();
      confirm.mockRestore();
    },
  );

  it("offers no GitHub sign out when access comes from GITLAB_TOKEN", async () => {
    client.setQueryData(["setup", "status"], {
      ...client.getQueryData<object>(["setup", "status"]),
      auth: {
        loggedIn: true,
        detail: "glab authenticated via GITLAB_TOKEN",
        login: "developer",
        host: "gitlab.example.com",
      },
    });
    await navigate("Integrações");
    const row = Array.from(container.querySelectorAll("#settings-integrations .settings-row")).find(
      (item) => item.querySelector("h3")?.textContent === "GitLab",
    )!;
    expect(row.textContent).toContain("GITLAB_TOKEN do ambiente");
    expect(Array.from(row.querySelectorAll("button")).some((item) => item.textContent === "Sair")).toBe(false);
  });

  it("gives the maximum MR files control an accessible name", () => {
    const input = container.querySelector<HTMLInputElement>('input[aria-label="Máximo de arquivos do MR"]');
    expect(input).not.toBeNull();
    expect(input?.value).toBe("100");
  });

  it("lets the number of reviews at a time be changed", async () => {
    await navigate("Seleção de MRs");
    const concurrency = container.querySelector<HTMLInputElement>('input[aria-label="Revisões ao mesmo tempo"]')!;
    await inputValue(concurrency, "8");
    await act(async () => button("Salvar alterações").click());
    expect(vi.mocked(setupApi.saveSettingsBatch).mock.calls[0][0]).toEqual({ settings: { review_concurrency: "8" } });
  });

  it("leaves suggestions off by default and saves turning them on", async () => {
    await navigate("Seleção de MRs");
    expect(container.textContent).toContain("Publicar sugestões");
    expect(button("Desligado").classList.contains("active")).toBe(true);
    await act(async () => button("Ligado").click());
    expect(button("Ligado").classList.contains("active")).toBe(true);
    await act(async () => button("Salvar alterações").click());
    expect(vi.mocked(setupApi.saveSettingsBatch).mock.calls[0][0]).toEqual({
      settings: { review_suggestions_enabled: "true" },
    });
    expect(client.getQueryData<Settings>(["setup", "settings", "review"])!.reviewSuggestionsEnabled).toBe(true);
  });

  it("keeps one global action pair and saves changes from every section together", async () => {
    const header = container.querySelector("[data-page-header]")!;
    expect(header.contains(button("Cancelar"))).toBe(true);
    expect(header.contains(button("Salvar alterações"))).toBe(true);
    expect(
      Array.from(container.querySelectorAll("button")).filter((item) => item.textContent === "Cancelar"),
    ).toHaveLength(1);
    await act(async () =>
      container.querySelector<HTMLInputElement>(`input[aria-label="${repos[1].fullName}"]`)!.click(),
    );
    await navigate("Seleção de MRs");
    await act(async () => button("Atribuídos").click());
    await inputValue(container.querySelector<HTMLInputElement>('[aria-label="Palavras-chave ignoradas"]')!, "renovate");
    expect(container.querySelectorAll(".settings-unsaved-dot")).toHaveLength(2);
    expect(setupApi.saveSettingsBatch).not.toHaveBeenCalled();
    await act(async () => button("Salvar alterações").click());
    expect(setupApi.saveSettingsBatch).toHaveBeenCalledTimes(1);
    const payload = vi.mocked(setupApi.saveSettingsBatch).mock.calls[0][0];
    expect(payload.repositories).toEqual({ logins: ["example"], repoIds: [1, 2], monitorAll: [] });
    expect(payload.settings).toMatchObject({ review_assigned_only: "true", auto_skip_keywords: '["deps","renovate"]' });
    expect(payload.settings).not.toHaveProperty("review_routing");
    expect(container.querySelectorAll(".settings-unsaved-dot")).toHaveLength(0);
    expect(button("Salvar alterações").disabled).toBe(true);
    expect(container.textContent).toContain("Todas as alterações salvas.");
  });

  it("monitors every repository of an organization, including future ones", async () => {
    const all = await vi.waitFor(
      () =>
        container.querySelector<HTMLInputElement>('input[aria-label="Monitorar todos os repositórios de example"]')!,
    );
    const second = () => container.querySelector<HTMLInputElement>(`input[aria-label="${repos[1].fullName}"]`)!;
    expect(second().checked).toBe(false);
    await act(async () => all.click());
    expect(second().checked).toBe(true);
    expect(second().disabled).toBe(true);
    await act(async () => button("Salvar alterações").click());
    const payload = vi.mocked(setupApi.saveSettingsBatch).mock.calls[0][0];
    expect(payload.repositories).toEqual({ logins: ["example"], repoIds: [1], monitorAll: ["example"] });
  });

  it("cancels the entire draft including keywords without writes", async () => {
    await navigate("Seleção de MRs");
    await inputValue(container.querySelector<HTMLInputElement>('[aria-label="Palavras-chave ignoradas"]')!, "renovate");
    await act(async () => button("Atribuídos").click());
    await act(async () => button("Cancelar").click());
    expect(button("Salvar alterações").disabled).toBe(true);
    expect(container.querySelectorAll(".settings-unsaved-dot")).toHaveLength(0);
    expect(container.querySelector<HTMLInputElement>('[aria-label="Palavras-chave ignoradas"]')!.value).toBe("");
    expect(setupApi.saveSettingsBatch).not.toHaveBeenCalled();
  });

  it("retains all edits after a failed save and retries the same batch", async () => {
    vi.mocked(setupApi.saveSettingsBatch).mockRejectedValueOnce(new Error("Invalid settings"));
    await navigate("Seleção de MRs");
    await act(async () => button("Atribuídos").click());
    await act(async () => button("5m").click());
    await act(async () => button("Salvar alterações").click());
    expect(container.textContent).toContain("Suas alterações continuam pendentes");
    expect(button("Salvar alterações").disabled).toBe(false);
    expect(client.getQueryData<Settings>(["setup", "settings", "review"])!.reviewAssignedOnly).toBe(false);
    expect(container.querySelectorAll(".settings-unsaved-dot")).toHaveLength(1);
    await act(async () => button("Salvar alterações").click());
    const calls = vi.mocked(setupApi.saveSettingsBatch).mock.calls;
    expect(calls).toHaveLength(2);
    expect(calls[1][0]).toEqual(calls[0][0]);
    expect(calls[0][0].repositories).toBeUndefined();
    expect(button("Salvar alterações").disabled).toBe(true);
  });

  it("blocks duplicate saves, cancellation and edits while the batch is pending", async () => {
    let finish!: (value: Awaited<ReturnType<typeof setupApi.saveSettingsBatch>>) => void;
    vi.mocked(setupApi.saveSettingsBatch).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await navigate("Seleção de MRs");
    await act(async () => button("Atribuídos").click());
    const saveButton = button("Salvar alterações");
    await act(async () => {
      saveButton.click();
      saveButton.click();
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(button("Salvando…").disabled).toBe(true);
    expect(button("Cancelar").disabled).toBe(true);
    expect(button("Todos os MRs").disabled).toBe(true);
    await act(async () => button("Salvando…").click());
    expect(setupApi.saveSettingsBatch).toHaveBeenCalledTimes(1);
    await act(async () =>
      finish({
        settings: { ...client.getQueryData<Settings>(["setup", "settings", "review"])!, reviewAssignedOnly: true },
        workspaces: [workspace],
        selectedRepos: [],
      }),
    );
    expect(button("Salvar alterações").disabled).toBe(true);
  });

  it("preserves drafts across refetches and only submits fields still changed", async () => {
    await navigate("Seleção de MRs");
    await act(async () => button("Atribuídos").click());
    await act(async () => {
      client.setQueryData<Settings>(["setup", "settings", "review"], (current) => ({
        ...current!,
        pollIntervalSeconds: 300,
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(button("Atribuídos").classList.contains("active")).toBe(true);
    expect(button("5m").classList.contains("active")).toBe(true);
    await act(async () => button("Todos os MRs").click());
    expect(button("Salvar alterações").disabled).toBe(true);
    await act(async () => {
      client.setQueryData<Settings>(["setup", "settings", "review"], (current) => ({
        ...current!,
        reviewAssignedOnly: true,
      }));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(button("Atribuídos").classList.contains("active")).toBe(true);
    expect(button("Salvar alterações").disabled).toBe(true);
  });

  it("includes blank numeric edits in the global draft", async () => {
    await navigate("Seleção de MRs");
    const maxFiles = container.querySelector<HTMLInputElement>('input[aria-label="Máximo de arquivos do MR"]')!;
    await inputValue(maxFiles, "");
    expect(maxFiles.value).toBe("");
    await inputValue(maxFiles, "40");
    await act(async () => button("Salvar alterações").click());
    expect(vi.mocked(setupApi.saveSettingsBatch).mock.calls[0][0]).toEqual({ settings: { review_max_files: "40" } });
    expect(button("Salvar alterações").disabled).toBe(true);
  });

  it("guards navigation with pending changes and clears the guard on cancel", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    const blocker = () =>
      vi.mocked(useBlocker).mock.calls.at(-1)![0] as unknown as {
        enableBeforeUnload: boolean;
        shouldBlockFn: () => boolean;
      };
    expect(blocker().enableBeforeUnload).toBe(false);
    await navigate("Seleção de MRs");
    await act(async () => button("Atribuídos").click());
    expect(blocker().enableBeforeUnload).toBe(true);
    expect(blocker().shouldBlockFn()).toBe(true);
    await act(async () => button("Cancelar").click());
    expect(blocker().enableBeforeUnload).toBe(false);
    confirm.mockRestore();
  });

  it("shows providers first and only one section at a time", async () => {
    const panels = () =>
      Array.from(container.querySelectorAll<HTMLElement>(".settings-section-panel")).filter((panel) => !panel.hidden);
    expect(panels()).toHaveLength(1);
    expect(panels()[0].textContent).toContain("Provedores");
    const sections = Array.from(container.querySelectorAll("nav[aria-label='Seções de configurações'] button")).map(
      (item) => item.textContent,
    );
    expect(sections).toEqual(["Provedores", "Acesso a repositórios", "Seleção de MRs", "Integrações"]);
    for (const label of ["Acesso a repositórios", "Seleção de MRs", "Integrações"]) {
      await navigate(label);
      expect(panels()).toHaveLength(1);
      expect(panels()[0].textContent).toContain(label);
      expect(container.querySelector("nav button[aria-current='true']")?.textContent).toContain(label);
    }
  });

  it("searches repository rows and preserves unsaved selection across sections", async () => {
    await navigate("Acesso a repositórios");
    const search = container.querySelector<HTMLInputElement>("input[aria-label='Buscar repositórios']");
    expect(search).not.toBeNull();
    expect(container.querySelectorAll(".settings-repository-option")).toHaveLength(2);
    await inputValue(search!, "another");
    expect(container.querySelectorAll(".settings-repository-option")).toHaveLength(1);
    const checkbox = container.querySelector<HTMLInputElement>(".settings-repository-option input")!;
    await act(async () => checkbox.click());
    expect(checkbox.checked).toBe(true);
    expect(button("Salvar alterações").disabled).toBe(false);
    await navigate("Seleção de MRs");
    await navigate("Acesso a repositórios");
    expect(container.querySelector<HTMLInputElement>(".settings-repository-option input")!.checked).toBe(true);
    await act(async () => button("Cancelar").click());
    expect(container.querySelector<HTMLInputElement>(".settings-repository-option input")!.checked).toBe(false);
    expect(button("Salvar alterações").disabled).toBe(true);
    await inputValue(search!, "no-such-repository");
    expect(container.textContent).toContain("Nenhum repositório corresponde à busca.");
  });
});
