import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import ForceGraph2D, { type ForceGraphProps } from "react-force-graph-2d";
import { describe, expect, it, vi } from "vitest";
import type { IndexGraph, IndexGraphNode, RepoIndexStatus } from "../../types";
import { GraphEmpty, IndexGraphView } from "./IndexGraph";

vi.mock("../../services/setupApi", () => ({ setupApi: {} }));
vi.mock("react-force-graph-2d", () => ({ default: vi.fn(() => <canvas data-testid="force-graph" />) }));
const networkProps = () =>
  vi.mocked(ForceGraph2D).mock.calls.at(-1)![0] as ForceGraphProps<IndexGraphNode, { dev: boolean }>;
const graph: IndexGraph = {
  nodes: [
    { id: "a", name: "Core", path: "packages/core", kind: "lib" },
    { id: "b", name: "App", path: "apps/web", kind: "app" },
  ],
  edges: [{ source: "b", target: "a", dev: true }],
};
const noop = () => undefined;

function renderGraph(
  state: RepoIndexStatus["status"],
  data = graph,
  selectedId: string | null = null,
  onSelect: (id: string | null) => void = noop,
) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(["indexing", "graph", 1, "packages", null], data);
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <IndexGraphView
        repoId={1}
        repoName="team/repo"
        status={{ status: state } as RepoIndexStatus}
        selectedId={selectedId}
        onSelect={onSelect}
      />
    </QueryClientProvider>,
  );
  client.clear();
  return html;
}

function SelectableGraph({ client }: { client: QueryClient }) {
  const [selectedId, onSelect] = useState<string | null>(null);
  return (
    <QueryClientProvider client={client}>
      <IndexGraphView
        repoId={1}
        repoName="team/repo"
        status={{ status: "ready" } as RepoIndexStatus}
        selectedId={selectedId}
        onSelect={onSelect}
      />
    </QueryClientProvider>
  );
}

describe("index graph view", () => {
  it("uses the animated lens while loading the saved graph", () => {
    const client = new QueryClient({ defaultOptions: { queries: { enabled: false } } });
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <IndexGraphView
          repoId={1}
          repoName="team/repo"
          status={{ status: "ready" } as RepoIndexStatus}
          selectedId={null}
          onSelect={noop}
        />
      </QueryClientProvider>,
    );
    const document = new DOMParser().parseFromString(html, "text/html");
    expect(document.querySelector('.brand-loading svg[data-motion="loading"]')).not.toBeNull();
    expect(document.querySelector('[role="status"] p')?.textContent).toBe("Reading the saved repository index…");
    client.clear();
  });

  it("measures the fixed viewport instead of feeding canvas height back into itself", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const observe = vi.fn(),
      disconnect = vi.fn();
    const instance = {
      getGraphBbox: vi.fn(() => ({ x: [NaN, NaN], y: [NaN, NaN] })),
      zoomToFit: vi.fn(),
      zoom: vi.fn(() => 1),
    };
    vi.mocked(ForceGraph2D).mockImplementation((props) => {
      if (props.ref) props.ref.current = instance as unknown as typeof props.ref.current;
      return <canvas data-testid="force-graph" />;
    });
    let resize: () => void = noop;
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: () => void) {
          resize = callback;
        }
        observe = observe;
        disconnect = disconnect;
      },
    );
    let height = 400;
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (this: HTMLElement) {
      return this.classList.contains("index-flow-canvas") ? 800 : 9000;
    });
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) {
      return this.classList.contains("index-flow-canvas") ? height : 9000;
    });
    const client = new QueryClient({ defaultOptions: { queries: { enabled: false } } });
    client.setQueryData(["indexing", "graph", 1, "packages", null], graph);
    const element = document.createElement("div");
    const root = createRoot(element);
    try {
      await act(async () =>
        root.render(
          <QueryClientProvider client={client}>
            <IndexGraphView
              repoId={1}
              repoName="team/repo"
              status={{ status: "ready" } as RepoIndexStatus}
              selectedId={null}
              onSelect={noop}
            />
          </QueryClientProvider>,
        ),
      );
      expect(observe).toHaveBeenCalledWith(element.querySelector(".index-flow-canvas"));
      expect(networkProps()).toMatchObject({ width: 800, height: 400 });
      expect(instance.zoomToFit).not.toHaveBeenCalled();
      instance.getGraphBbox.mockReturnValue({ x: [-4, 4], y: [-4, 4] });
      await act(async () => networkProps().onEngineStop?.());
      expect(instance.zoomToFit).toHaveBeenCalledWith(0, 64);
      height = 260;
      await act(async () => resize());
      expect(networkProps()).toMatchObject({ width: 800, height: 260 });
    } finally {
      await act(async () => root.unmount());
      expect(disconnect).toHaveBeenCalledOnce();
      client.clear();
      vi.mocked(ForceGraph2D).mockImplementation(() => <canvas data-testid="force-graph" />);
      vi.restoreAllMocks();
      vi.unstubAllGlobals();
    }
  });
  it("shows the graph inline, not inside a modal", () => {
    const html = renderGraph("ready");
    expect(html).toContain('aria-label="Grafo interativo de pacotes"');
    expect(html).toContain("2 packages");
    expect(html).not.toContain('role="dialog"');
    expect(html).not.toContain("Mostrando o último índice salvo");
  });
  it("uses the point-and-line graph while keeping navigation and keyboard selection", () => {
    const html = renderGraph("ready");
    expect(html).toContain('data-testid="force-graph"');
    expect(html).not.toContain("react-flow");
    expect(html).toContain('aria-label="Local do grafo"');
    expect(html).toContain('aria-label="Buscar pacotes"');
    expect(html).toContain('aria-label="Selecionar nó do grafo"');
    expect(html).toContain("Ajustar grafo");
  });
  it("preserves dependency direction and dev styling without mutating cached data", () => {
    renderGraph("ready");
    const props = networkProps();
    expect(props.graphData?.links).toEqual(graph.edges);
    expect(props.linkDirectionalArrowLength).toBeGreaterThan(0);
    const dash = props.linkLineDash;
    expect(typeof dash).toBe("function");
    if (typeof dash === "function") {
      expect(dash(graph.edges[0])).toEqual([4, 3]);
      expect(dash({ ...graph.edges[0], dev: false })).toBeNull();
    }
    const data = props.graphData!;
    data.nodes[0].x = 123;
    data.links[0].source = data.nodes[0];
    expect(graph.nodes.every((node) => !("x" in node))).toBe(true);
    expect(graph.edges[0].source).toBe("b");
  });
  it.each(["canvas", "keyboard", "search"])(
    "explores packages and files inline through %s without an inspector",
    async (entry) => {
      vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
      vi.stubGlobal(
        "ResizeObserver",
        class {
          observe() {}
          disconnect() {}
        },
      );
      const pkg = { id: "1", name: "Core", path: "packages/core", kind: "lib" };
      const file = { id: "2", name: "index.ts", path: "packages/core/index.ts", kind: "typescript" };
      const symbol = { id: "3", name: "run", path: file.path, kind: "function" };
      const client = new QueryClient({ defaultOptions: { queries: { enabled: false } } });
      client.setQueryData(["indexing", "graph", 1, "packages", null], { nodes: [pkg], edges: [] });
      client.setQueryData(["indexing", "graph", 1, "files", pkg.id], { nodes: [file], edges: [] });
      client.setQueryData(["indexing", "graph", 1, "symbols", null], { nodes: [symbol], edges: [] });
      const element = document.createElement("div");
      const root = createRoot(element);
      const activate = async (node: IndexGraphNode) => {
        if (entry === "canvas") await act(async () => networkProps().onNodeClick?.(node, {} as MouseEvent));
        else if (entry === "keyboard")
          await act(async () => {
            const select = element.querySelector("select")!;
            select.value = node.id;
            select.dispatchEvent(new Event("change", { bubbles: true }));
          });
        else {
          await act(async () => {
            const input = element.querySelector("input")!;
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, node.name);
            input.dispatchEvent(new Event("input", { bubbles: true }));
          });
          await act(async () => element.querySelector<HTMLButtonElement>(".index-search-results button")!.click());
        }
      };
      try {
        await act(async () => root.render(<SelectableGraph client={client} />));
        await activate(pkg);
        expect(element.querySelector('[aria-label="Grafo interativo de arquivos"]')).not.toBeNull();
        expect(networkProps().graphData?.nodes).toEqual([file]);
        expect(element.querySelector('[aria-label="Code details"]')).toBeNull();
        expect(element.querySelector(".index-breadcrumbs")?.textContent).toContain("Core");
        await activate(file);
        expect(element.querySelector('[aria-label="Grafo interativo de símbolos"]')).not.toBeNull();
        expect(networkProps().graphData?.nodes).toEqual([symbol]);
        expect(element.querySelector('[aria-label="Code details"]')).toBeNull();
        expect(element.querySelector(".index-breadcrumbs")?.textContent).toContain("index.ts");
        await activate(symbol);
        expect(element.querySelector('[aria-label="Code details"]')).toBeNull();
        expect(element.textContent).toContain("Focused on direct relationships");
        await act(async () => element.querySelectorAll<HTMLButtonElement>(".index-breadcrumbs button")[1].click());
        expect(element.querySelector('[aria-label="Grafo interativo de arquivos"]')).not.toBeNull();
        await act(async () => element.querySelector<HTMLButtonElement>(".index-breadcrumbs button")!.click());
        expect(element.querySelector('[aria-label="Grafo interativo de pacotes"]')).not.toBeNull();
      } finally {
        await act(async () => root.unmount());
        client.clear();
        vi.unstubAllGlobals();
      }
    },
  );
  it("keeps selection highlights without opening an inspector and highlights resolved links", () => {
    const select = vi.fn();
    const html = renderGraph("ready", graph, "a", select);
    expect(html).not.toContain('aria-label="Code details"');
    expect(html).toContain('value="a" selected=""');
    const props = networkProps();
    props.onBackgroundClick?.({} as MouseEvent);
    expect(select).toHaveBeenLastCalledWith(null);
    if (typeof props.linkWidth === "function") {
      expect(props.linkWidth({ source: graph.nodes[1], target: graph.nodes[0], dev: true })).toBe(1.8);
      expect(props.linkWidth({ source: "other", target: "another", dev: false })).toBe(1.1);
    }
  });
  it("renders a lone package without inventing connections and curves self references", () => {
    expect(renderGraph("ready", { nodes: [graph.nodes[0]], edges: [] })).toContain("1 packages · 0 dependencies");
    expect(networkProps().graphData?.links).toEqual([]);
    const curvature = networkProps().linkCurvature;
    if (typeof curvature === "function") expect(curvature({ source: "a", target: "a", dev: false })).toBe(0.6);
  });
  it.each(["indexing", "failed", "stale", "cancelled"] as const)("keeps saved graph available when %s", (state) => {
    const html = renderGraph(state);
    expect(html).toContain("Mostrando o último índice salvo");
    expect(html).toContain("2 packages");
  });
  it("does not claim an existing snapshot for a first index", () => {
    const html = renderGraph("indexing", { nodes: [], edges: [] });
    expect(html).toContain("The first index is being prepared");
    expect(html).toContain('class="brand-loading"');
    expect(html).not.toContain("Mostrando o último índice salvo");
  });
  it("renders actionable errors as text, not markup", () => {
    const html = renderToStaticMarkup(
      <GraphEmpty title="Graph unavailable" description="<script>failed</script>">
        <button type="button">Tentar grafo de novo</button>
      </GraphEmpty>,
    );
    expect(html).toContain("Tentar grafo de novo");
    expect(html).toContain("&lt;script&gt;");
  });
});
