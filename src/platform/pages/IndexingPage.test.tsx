import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { type IndexJob, type IndexJobsResponse, setupApi } from "../../services/setupApi";
import type { RepoIndexStatus } from "../../types";
import { IndexingPage } from "./IndexingPage";

vi.mock("../../services/setupApi", () => ({ setupApi: { reindexRepo: vi.fn(), listIndexJobs: vi.fn() } }));
vi.mock("../components/IndexGraph", () => ({ IndexGraphView: () => <div>Graph</div> }));

const status = (repoId: number, state: RepoIndexStatus["status"]) =>
  ({ repoId, status: state, indexedAt: null, baseSha: null }) as RepoIndexStatus;
const job = (id: number, state: RepoIndexStatus["status"]): IndexJob => ({
  repo: { id, fullName: `team/repo-${id}` } as IndexJob["repo"],
  status: status(id, state),
});
let client: QueryClient;
let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
  vi.mocked(setupApi.reindexRepo)
    .mockReset()
    .mockImplementation(async (id) => status(id, "queued"));
  client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  client.clear();
  vi.unstubAllGlobals();
});

async function render(jobs: IndexJob[]) {
  client.setQueryData(["indexing", "jobs"], { jobs, active: 0 });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <IndexingPage />
      </QueryClientProvider>,
    ),
  );
}

function button(label: string) {
  const element = Array.from(container.querySelectorAll("button")).find((item) => item.textContent === label);
  expect(element, label).toBeDefined();
  return element!;
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe("indexing workspace", () => {
  it("uses the animated lens for initial loading and keeps repositories visible during refetch", async () => {
    client.setDefaultOptions({ queries: { retry: false, staleTime: Infinity } });
    let finish!: (value: IndexJobsResponse) => void;
    vi.mocked(setupApi.listIndexJobs).mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await act(async () =>
      root.render(
        <QueryClientProvider client={client}>
          <IndexingPage />
        </QueryClientProvider>,
      ),
    );
    expect(container.querySelectorAll(".brand-loading")).toHaveLength(1);
    expect(container.querySelector('#index-repositories .brand-loading svg[data-motion="loading"]')).not.toBeNull();
    expect(container.querySelector(".review-workspace-main .brand-loading")).toBeNull();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Ocultar repositórios"]')!.click());
    expect(container.querySelectorAll(".brand-loading")).toHaveLength(1);
    expect(container.querySelector(".review-workspace-main .brand-loading")).not.toBeNull();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Mostrar repositórios"]')!.click());
    expect(container.querySelectorAll(".brand-loading")).toHaveLength(1);
    expect(container.querySelector(".review-workspace-main .brand-loading")).toBeNull();
    await act(async () => finish({ jobs: [job(1, "ready")], active: 0 }));
    await flush();
    expect(container.querySelector(".brand-loading")).toBeNull();
    expect(container.querySelectorAll(".index-repo-item")).toHaveLength(1);
    await act(async () => {
      void client.invalidateQueries({ queryKey: ["indexing", "jobs"] });
    });
    expect(container.querySelector(".brand-loading")).toBeNull();
    expect(container.querySelectorAll(".index-repo-item")).toHaveLength(1);
    await act(async () => finish({ jobs: [job(1, "ready")], active: 0 }));
    await flush();
  });

  it.each([
    [true, true],
    [true, false],
    [false, true],
    [false, false],
  ])(
    "keeps icon toggles inside the panel and preserves keyboard focus (wide: %s, populated: %s)",
    async (wide, populated) => {
      vi.stubGlobal("matchMedia", () => ({ matches: wide, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
      await render(populated ? [job(1, "ready")] : []);
      const panel = container.querySelector<HTMLElement>("#index-repositories")!;
      const toggle = (label: string) => {
        const element = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
        expect(element).not.toBeNull();
        expect(element!.textContent).toBe("");
        expect(element!.title).toBe(label);
        expect(element!.getAttribute("aria-controls")).toBe("index-repositories");
        expect(element!.closest(".index-workspace")).not.toBeNull();
        return element!;
      };
      expect(container.querySelector('.index-workspace-summary [aria-controls="index-repositories"]')).toBeNull();
      expect(panel.hidden).toBe(!wide);
      if (!wide) await act(async () => toggle("Mostrar repositórios").click());
      const close = toggle("Ocultar repositórios");
      expect(panel.contains(close)).toBe(true);
      expect(close.getAttribute("aria-expanded")).toBe("true");
      await act(async () => close.click());
      expect(panel.hidden).toBe(true);
      const open = toggle("Mostrar repositórios");
      expect(open.getAttribute("aria-expanded")).toBe("false");
      expect(open.closest(".review-workspace-main")).not.toBeNull();
      expect(document.activeElement).toBe(open);
      await act(async () => open.click());
      expect(panel.hidden).toBe(false);
      expect(document.activeElement).toBe(close);
      await act(async () =>
        panel.querySelector("input")!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })),
      );
      expect(panel.hidden).toBe(true);
      expect(document.activeElement).toBe(toggle("Mostrar repositórios"));
    },
  );

  it("places status beside repository metadata, not in a third line", async () => {
    await render([job(1, "ready")]);
    expect(container.querySelector(".index-repo-item > .index-repo-state")?.textContent).toBe("Pronto");
    expect(container.querySelector(".index-repo-owner")?.textContent).toBe("team");
  });

  it("reindexes all eligible repositories even when search filters the list", async () => {
    await render([job(1, "ready"), job(2, "failed"), job(3, "ready"), job(4, "queued"), job(5, "indexing")]);
    const input = container.querySelector<HTMLInputElement>("input[aria-label='Buscar repositórios']")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "repo-1");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.querySelectorAll(".index-repo-item")).toHaveLength(1);
    await act(async () => button("Reindexar todos").click());
    await flush();
    expect(vi.mocked(setupApi.reindexRepo).mock.calls.map(([id]) => id)).toEqual([1, 2, 3]);
    expect(
      client
        .getQueryData<IndexJobsResponse>(["indexing", "jobs"])
        ?.jobs.slice(0, 3)
        .map((item) => item.status.status),
    ).toEqual(["queued", "queued", "queued"]);
    expect(button("Reindexar todos").disabled).toBe(true);
  });

  it("shows progress and blocks duplicate requests while submitting the batch", async () => {
    let finish: (value: RepoIndexStatus) => void = () => undefined;
    vi.mocked(setupApi.reindexRepo).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await render([job(1, "ready"), job(2, "ready")]);
    await act(async () => button("Reindexar todos").click());
    await flush();
    expect(button("Iniciando 0/2…").disabled).toBe(true);
    expect(setupApi.reindexRepo).toHaveBeenCalledTimes(1);
    await act(async () => finish(status(1, "indexing")));
    await flush();
    expect(setupApi.reindexRepo).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("Reindexação pedida para 2 repositórios");
  });

  it("continues after a failure and keeps the error available for individual retry", async () => {
    vi.mocked(setupApi.reindexRepo).mockRejectedValueOnce(new Error("Cannot start indexing"));
    await render([job(1, "ready"), job(2, "ready")]);
    await act(async () => button("Reindexar todos").click());
    await flush();
    expect(setupApi.reindexRepo).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("1 não começou");
    expect(container.textContent).toContain("Cannot start indexing");
    await act(async () =>
      (container.querySelector(".index-repo-item[title='team/repo-2']") as HTMLButtonElement).click(),
    );
    await act(async () =>
      (container.querySelector(".index-repo-item[title='team/repo-1']") as HTMLButtonElement).click(),
    );
    expect(container.textContent).toContain("Cannot start indexing");
    await act(async () => button("Reindexar").click());
    await flush();
    expect(setupApi.reindexRepo).toHaveBeenLastCalledWith(1, expect.anything());
    expect(container.textContent).not.toContain("Cannot start indexing");
  });

  it("disables the bulk action when there is nothing eligible", async () => {
    await render([job(1, "queued"), job(2, "indexing")]);
    expect(button("Reindexar todos").disabled).toBe(true);
    await act(async () => client.setQueryData(["indexing", "jobs"], { jobs: [], active: 0 }));
    await flush();
    expect(button("Reindexar todos").disabled).toBe(true);
    expect(setupApi.reindexRepo).not.toHaveBeenCalled();
  });
});
