import { useQuery } from "@tanstack/react-query";
import { Box, Braces, ChevronRight, Maximize, Search } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ForceGraph2D, { type ForceGraphMethods, type LinkObject } from "react-force-graph-2d";
import { BrandLoading } from "../../components/BrandLoading";
import { BrandMark } from "../../components/BrandMark";
import { DSButton } from "../../components/DesignSystem";
import { setupApi } from "../../services/setupApi";
import type { IndexGraph, IndexGraphNode, RepoIndexStatus } from "../../types";
import { type GraphMode, graphSlice, normalizeGraph, searchNodes } from "../lib/indexGraph";

const emptyGraph: IndexGraph = { nodes: [], edges: [] };
type Scope = { mode: GraphMode; package: { id: string; name: string } | null; filePath: string | null };
const rootScope: Scope = { mode: "packages", package: null, filePath: null };

export function IndexGraphView({
  repoId,
  repoName,
  status,
  selectedId,
  onSelect,
}: {
  repoId: number;
  repoName: string;
  status: RepoIndexStatus;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
}) {
  const [scope, setScope] = useState<Scope>(rootScope);
  const [focusId, setFocusId] = useState<string | null>(null);
  const [includeIsolated, setIncludeIsolated] = useState(false);
  const [search, setSearch] = useState("");
  const [searchPage, setSearchPage] = useState(0);
  const graphQuery = useQuery({
    queryKey: ["indexing", "graph", repoId, scope.mode, scope.mode === "files" ? scope.package?.id : null],
    queryFn: () =>
      scope.mode === "symbols"
        ? setupApi.symbolGraph(repoId)
        : scope.mode === "files" && scope.package
          ? setupApi.indexPackageFiles(repoId, Number(scope.package.id))
          : setupApi.indexGraph(repoId),
    refetchOnWindowFocus: false,
  });
  const graph = useMemo(() => normalizeGraph(graphQuery.data ?? emptyGraph), [graphQuery.data]);
  const slice = useMemo(
    () =>
      graphSlice(graph, {
        focusId,
        filePath: scope.filePath,
        includeIsolated: scope.mode === "packages" || includeIsolated,
      }),
    [graph, focusId, scope.filePath, scope.mode, includeIsolated],
  );
  const selected = graph.nodes.find((node) => node.id === selectedId) ?? null;
  const nodes = slice.nodes;
  useEffect(() => {
    if (graphQuery.isSuccess && selectedId && !selected) onSelect(null);
  }, [graphQuery.isSuccess, selectedId, selected, onSelect]);
  const changeScope = (next: Scope) => {
    setScope(next);
    setFocusId(null);
    setSearch("");
    setSearchPage(0);
    setIncludeIsolated(false);
    onSelect(null);
  };
  const explore = (id: string | null) => {
    const node = graph.nodes.find((node) => node.id === id);
    if (!node) {
      onSelect(null);
      return;
    }
    if (scope.mode === "packages")
      changeScope({ mode: "files", package: { id: node.id, name: node.name }, filePath: null });
    else if (scope.mode === "files") changeScope({ mode: "symbols", package: scope.package, filePath: node.path });
    else {
      setFocusId(node.id);
      setSearch("");
      setSearchPage(0);
      onSelect(node.id);
    }
  };
  const searchPool = useMemo(
    () => (scope.filePath ? graph.nodes.filter((node) => node.path === scope.filePath) : graph.nodes),
    [graph.nodes, scope.filePath],
  );
  const matches = useMemo(() => searchNodes(searchPool, search), [searchPool, search]);
  const searchPages = Math.ceil(matches.length / 12);
  const currentPage = Math.min(searchPage, Math.max(0, searchPages - 1));
  const graphLabel = scope.mode === "packages" ? "dependencies" : scope.mode === "files" ? "imports" : "references";
  const outdated = status.status !== "ready" && graph.nodes.length > 0;
  return (
    <div className="index-explorer flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="index-graph-toolbar flex flex-none flex-col gap-3 border-b border-line bg-surface px-5 py-3.5">
        <div className="index-graph-navigation flex flex-wrap items-center justify-between gap-2.5">
          <nav
            className="index-breadcrumbs flex min-w-0 flex-1 items-center gap-1.5 text-caption text-fg-muted [&>svg]:flex-none [&>button]:truncate [&>span]:truncate [&_button]:cursor-pointer [&_button]:py-0.5 [&_button:hover]:text-fg"
            aria-label="Local do grafo"
          >
            <button type="button" title={repoName} onClick={() => changeScope(rootScope)}>
              {repoName.split("/").pop()}
            </button>
            {scope.package && (
              <>
                <ChevronRight className="size-3" />
                <button
                  type="button"
                  onClick={() => changeScope({ mode: "files", package: scope.package, filePath: null })}
                >
                  {scope.package.name}
                </button>
              </>
            )}
            {scope.filePath && (
              <>
                <ChevronRight className="size-3" />
                <span title={scope.filePath}>{scope.filePath.split("/").pop()}</span>
              </>
            )}
          </nav>
          {/* biome-ignore lint/a11y/useSemanticElements: a <fieldset> adds UA min-inline-size and border quirks to this inline toggle group */}
          <div
            className="index-view-switch inline-flex flex-none gap-0.5 rounded-lg bg-surface-muted p-1 [&_button]:flex [&_button]:cursor-pointer [&_button]:items-center [&_button]:gap-1.5 [&_button]:rounded-md [&_button]:px-2.5 [&_button]:py-1 [&_button]:text-caption [&_button]:font-medium [&_button]:text-fg-muted [&_button[aria-pressed=true]]:bg-surface [&_button[aria-pressed=true]]:text-brand [&_button[aria-pressed=true]]:shadow-raised"
            role="group"
            aria-label="Graph view"
          >
            <button
              type="button"
              aria-pressed={scope.mode !== "symbols"}
              onClick={() =>
                changeScope(scope.package ? { mode: "files", package: scope.package, filePath: null } : rootScope)
              }
            >
              <Box className="size-3.5" />
              {scope.package ? "Files" : "Packages"}
            </button>
            <button
              type="button"
              aria-pressed={scope.mode === "symbols"}
              onClick={() => changeScope({ mode: "symbols", package: null, filePath: null })}
            >
              <Braces className="size-3.5" />
              Symbols
            </button>
          </div>
        </div>
        <div className="index-graph-tools flex flex-wrap items-center justify-between gap-2.5">
          <div className="index-graph-search relative max-w-md min-w-36 flex-1">
            <div className="index-search-input flex h-9 items-center gap-2 rounded-md border border-line bg-surface px-2.5 text-fg-subtle shadow-card focus-within:border-brand focus-within:ring-3 focus-within:ring-brand/15 [&>svg]:flex-none [&_input]:w-full [&_input]:min-w-0 [&_input]:bg-transparent [&_input]:text-small [&_input]:text-fg [&_input]:outline-none [&>button]:cursor-pointer">
              <Search className="size-3.5" />
              <input
                id="index-graph-search"
                aria-label={`Buscar ${scope.mode === "packages" ? "pacotes" : scope.mode === "files" ? "arquivos" : "símbolos"}`}
                placeholder={`Buscar ${scope.mode === "packages" ? "pacotes" : scope.mode === "files" ? "arquivos" : "símbolos"} por nome ou caminho…`}
                value={search}
                onChange={(event) => {
                  setSearch(event.target.value);
                  setSearchPage(0);
                }}
                onKeyDown={(event) => {
                  if (event.key === "Escape") setSearch("");
                }}
              />
              {search && (
                <button type="button" onClick={() => setSearch("")} aria-label="Limpar busca do grafo">
                  ×
                </button>
              )}
            </div>
            {search.trim() && (
              <div className="index-search-results absolute top-[calc(100%+6px)] left-0 z-20 max-h-[min(440px,55dvh)] w-[max(100%,280px)] max-w-[calc(100vw-32px)] overflow-auto rounded-xl border border-line bg-surface p-1.5 shadow-pop [&>button]:grid [&>button]:w-full [&>button]:cursor-pointer [&>button]:gap-0.5 [&>button]:rounded-md [&>button]:p-2 [&>button]:text-left [&>button:hover]:bg-surface-muted [&>button>span]:text-small [&>button>span]:text-fg [&_code]:truncate [&_code]:font-mono [&_code]:text-caption [&_code]:text-fg-subtle [&>p]:p-2 [&>p]:text-caption [&>p]:text-fg-subtle">
                <div className="index-search-caption p-2 text-caption text-fg-subtle">
                  {matches.length} results in {scope.filePath ? "this file" : "this scope"}
                </div>
                {matches.slice(currentPage * 12, (currentPage + 1) * 12).map((node) => (
                  <button type="button" key={node.id} title={node.path} onClick={() => explore(node.id)}>
                    <span>{node.name}</span>
                    <code>{node.path || "Repository root"}</code>
                  </button>
                ))}
                {matches.length === 0 && <p>No matching {scope.mode}.</p>}
                {searchPages > 1 && (
                  <div className="index-pagination mt-2.5 flex items-center justify-between gap-1.5 text-caption text-fg-muted">
                    <DSButton disabled={currentPage === 0} onClick={() => setSearchPage(currentPage - 1)}>
                      Previous
                    </DSButton>
                    <span>
                      {currentPage + 1} / {searchPages}
                    </span>
                    <DSButton disabled={currentPage === searchPages - 1} onClick={() => setSearchPage(currentPage + 1)}>
                      Next
                    </DSButton>
                  </div>
                )}
              </div>
            )}
          </div>
          {focusId ? (
            <DSButton
              onClick={() => {
                setFocusId(null);
                onSelect(null);
              }}
            >
              Back to {scope.filePath ? "file symbols" : scope.mode}
            </DSButton>
          ) : (
            scope.mode !== "packages" &&
            slice.isolatedCount > 0 && (
              <label className="index-isolated-toggle flex cursor-pointer items-center gap-1.5 text-caption text-fg-muted [&_input]:accent-brand">
                <input
                  type="checkbox"
                  checked={includeIsolated}
                  onChange={(event) => setIncludeIsolated(event.target.checked)}
                />
                Show isolated
              </label>
            )
          )}
        </div>
      </div>
      {outdated && (
        <div
          className="index-snapshot-notice flex-none border-b border-amber-200 bg-amber-50 px-5 py-2 text-small text-amber-900"
          role="status"
        >
          Mostrando o último índice salvo.{" "}
          {status.status === "indexing" || status.status === "queued"
            ? "A fresh index is being prepared."
            : "Re-index to refresh this context."}
        </div>
      )}
      {graphQuery.isError && graphQuery.data && (
        <div className="index-job-error" role="alert">
          Não foi possível atualizar o grafo: {graphQuery.error.message}{" "}
          <DSButton onClick={() => void graphQuery.refetch()}>Tentar de novo</DSButton>
        </div>
      )}
      <div className="index-graph-body relative flex min-h-0 min-w-0 flex-1">
        <div className="index-graph-center flex min-h-0 min-w-0 flex-1 flex-col">
          <section
            className="index-flow-canvas relative min-h-0 min-w-0 flex-1 overflow-hidden bg-canvas [&>.brand-loading]:absolute [&>.brand-loading]:inset-0"
            aria-label={`Grafo interativo de ${scope.mode === "packages" ? "pacotes" : scope.mode === "files" ? "arquivos" : "símbolos"}`}
          >
            {graphQuery.isPending ? (
              <BrandLoading label="Reading the saved repository index…" />
            ) : graphQuery.isError && !graphQuery.data ? (
              <GraphEmpty title="Graph unavailable" description={graphQuery.error.message}>
                <DSButton onClick={() => void graphQuery.refetch()}>Tentar grafo de novo</DSButton>
              </GraphEmpty>
            ) : nodes.length === 0 && (status.status === "indexing" || status.status === "queued") ? (
              <BrandLoading label="The first index is being prepared. The graph will appear when it completes." />
            ) : nodes.length === 0 && status.status === "empty" ? (
              <GraphEmpty
                title="Ainda sem commits"
                description={`Push to ${repoName} and it will be indexed automatically.`}
              />
            ) : nodes.length === 0 ? (
              <GraphEmpty
                title={
                  scope.mode === "symbols"
                    ? "Nenhum símbolo neste escopo"
                    : scope.mode === "files"
                      ? "Nenhum arquivo neste pacote"
                      : "Nenhum pacote indexado"
                }
                description={
                  status.status === "failed"
                    ? "Indexing failed. Use Retry indexing above to build this repository's context."
                    : "Re-index this repository to refresh its context, or explore another graph view."
                }
              />
            ) : (
              <IndexNetwork
                key={`${scope.mode}:${scope.package?.id}:${scope.filePath}:${focusId}:${includeIsolated}`}
                graph={slice}
                mode={scope.mode}
                selectedId={selectedId}
                onSelect={explore}
              />
            )}
            {nodes.length > 0 && (
              <div className="index-flow-hint pointer-events-none absolute top-3.5 left-5 text-caption text-fg-subtle">
                {focusId
                  ? "Focused on direct relationships"
                  : scope.mode === "packages"
                    ? "Selecione um pacote para explorar os arquivos"
                    : scope.mode === "files"
                      ? "Selecione um arquivo para explorar os símbolos"
                      : "Selecione um símbolo para explorar as conexões"}
              </div>
            )}
          </section>
          <footer className="index-graph-footer flex flex-none flex-wrap items-center justify-between gap-x-4 gap-y-1.5 border-t border-line bg-surface px-5 py-2.5 text-caption text-fg-muted [&>span:last-child]:text-fg-subtle">
            <span>
              {slice.nodes.length} {scope.mode} · {slice.edges.length} {graphLabel}
              {slice.hiddenIsolated > 0 && ` · ${slice.hiddenIsolated} isolated hidden`}
            </span>
            {slice.omittedNodes > 0 || slice.omittedEdges > 0 ? (
              <span>
                {slice.omittedNodes} nodes / {slice.omittedEdges} edges omitted · search to focus
              </span>
            ) : (
              <span>
                {scope.mode === "files"
                  ? "Within this package"
                  : scope.mode === "symbols"
                    ? "Lightweight symbol index"
                    : "Internal dependencies"}
              </span>
            )}
          </footer>
        </div>
      </div>
    </div>
  );
}

const kindColors: Record<GraphMode, Record<string, string>> = {
  packages: { app: "--accent-hover", lib: "--success", config: "--success", tool: "--warning", root: "--text-2" },
  files: {
    typescript: "--accent-hover",
    typescriptreact: "--success",
    javascript: "--warning",
    javascriptreact: "--danger",
    rust: "--danger",
    python: "--info",
    go: "--success",
    ruby: "--danger",
    styles: "--accent",
    markdown: "--text-2",
    json: "--success",
    toml: "--accent",
    yaml: "--info",
    markup: "--success",
  },
  symbols: {
    function: "--accent-hover",
    class: "--success",
    method: "--info",
    interface: "--warning",
    type: "--accent",
    enum: "--warning",
    const: "--text-2",
    struct: "--success",
  },
};
const token = (name: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
type NetworkLink = LinkObject<IndexGraphNode, { dev: boolean }>;
const endpointId = (endpoint: NetworkLink["source"]) => (typeof endpoint === "object" ? endpoint.id : endpoint);

function IndexNetwork({
  graph,
  mode,
  selectedId,
  onSelect,
}: {
  graph: IndexGraph;
  mode: GraphMode;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
}) {
  const ref = useRef<ForceGraphMethods<IndexGraphNode, { dev: boolean }> | undefined>(undefined);
  const containerRef = useRef<HTMLDivElement>(null);
  const needsFit = useRef(true);
  const [dims, setDims] = useState({ width: 900, height: 620 });
  const [hoverId, setHoverId] = useState<string | null>(null);
  const data = useMemo(
    () => ({ nodes: graph.nodes.map((node) => ({ ...node })), links: graph.edges.map((edge) => ({ ...edge })) }),
    [graph],
  );
  const activeId = hoverId ?? selectedId;
  const neighbors = useMemo(
    () =>
      new Set(
        graph.edges
          .filter((edge) => edge.source === activeId || edge.target === activeId)
          .flatMap((edge) => [edge.source, edge.target]),
      ),
    [graph, activeId],
  );
  const legend = useMemo(() => [...new Set(graph.nodes.map((node) => node.kind))].sort(), [graph]);
  const fitToView = useCallback(() => {
    const instance = ref.current;
    const bounds = instance?.getGraphBbox();
    if (!instance || !bounds || ![...bounds.x, ...bounds.y].every(Number.isFinite)) return;
    instance.zoomToFit(0, 64);
    if (instance.zoom() > 3) instance.zoom(3, 0);
  }, []);
  useEffect(() => {
    const element = containerRef.current?.parentElement;
    if (!element) return;
    const update = () =>
      setDims((current) => {
        const width = Math.max(1, element.clientWidth),
          height = Math.max(1, element.clientHeight);
        return current.width === width && current.height === height ? current : { width, height };
      });
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  // biome-ignore lint/correctness/useExhaustiveDependencies: data is the trigger: a new graph must be refit once the engine stops
  useEffect(() => {
    needsFit.current = true;
  }, [data]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: dims is the trigger: refit the graph whenever the canvas is resized
  useEffect(() => {
    fitToView();
  }, [dims, fitToView]);
  const connected = (link: NetworkLink) =>
    !activeId || endpointId(link.source) === activeId || endpointId(link.target) === activeId;
  return (
    <div ref={containerRef} className="index-network absolute inset-0 overflow-hidden">
      <ForceGraph2D<IndexGraphNode, { dev: boolean }>
        ref={ref}
        graphData={data}
        width={dims.width}
        height={dims.height}
        backgroundColor="transparent"
        minZoom={0.1}
        maxZoom={8}
        warmupTicks={60}
        cooldownTicks={100}
        nodeLabel={() => ""}
        linkColor={(link) => token(connected(link) ? (activeId ? "--accent-hover" : "--text-3") : "--border")}
        linkWidth={(link) => (connected(link) && activeId ? 1.8 : 1.1)}
        linkLineDash={(link) => (link.dev ? [4, 3] : null)}
        linkCurvature={(link) => (endpointId(link.source) === endpointId(link.target) ? 0.6 : link.dev ? 0.2 : 0.08)}
        linkDirectionalArrowLength={3}
        linkDirectionalArrowRelPos={1}
        onEngineStop={() => {
          if (needsFit.current) {
            needsFit.current = false;
            fitToView();
          }
        }}
        onNodeHover={(node) => setHoverId(node?.id ?? null)}
        onNodeClick={(node) => onSelect(node.id)}
        onBackgroundClick={() => onSelect(null)}
        nodePointerAreaPaint={(node, color, ctx) => {
          ctx.fillStyle = color;
          ctx.beginPath();
          ctx.arc(node.x ?? 0, node.y ?? 0, 8, 0, Math.PI * 2);
          ctx.fill();
        }}
        nodeCanvasObject={(node, ctx, scale) => {
          const active = node.id === activeId;
          const dimmed = activeId !== null && !active && !neighbors.has(node.id);
          const radius = active ? 6.5 : 4;
          ctx.globalAlpha = dimmed ? 0.18 : 1;
          ctx.beginPath();
          ctx.arc(node.x ?? 0, node.y ?? 0, radius, 0, Math.PI * 2);
          ctx.fillStyle = token(kindColors[mode][node.kind] ?? "--text-2");
          ctx.fill();
          if (active) {
            ctx.lineWidth = 1.8 / scale;
            ctx.strokeStyle = token("--text-1");
            ctx.stroke();
          }
          if (!dimmed && (graph.nodes.length <= 30 || scale > 2.4 || active || neighbors.has(node.id))) {
            ctx.font = `${11 / scale}px ui-sans-serif, system-ui, sans-serif`;
            ctx.textAlign = "center";
            ctx.textBaseline = "top";
            const y = (node.y ?? 0) + radius + 2;
            ctx.lineWidth = 3 / scale;
            ctx.strokeStyle = token("--bg");
            ctx.strokeText(node.name, node.x ?? 0, y);
            ctx.fillStyle = token("--text-1");
            ctx.fillText(node.name, node.x ?? 0, y);
          }
          ctx.globalAlpha = 1;
        }}
      />
      {hoverId && (
        <div className="index-network-tooltip pointer-events-none absolute bottom-15 left-5 max-w-[calc(100%-40px)] rounded-md border border-line bg-surface px-2.5 py-1.5 text-caption text-fg-muted shadow-raised [overflow-wrap:anywhere]">
          {graph.nodes.find((node) => node.id === hoverId)?.path || "Repository root"}
        </div>
      )}
      <div className="index-network-controls absolute bottom-4 left-5 flex max-w-[calc(100%-40px)] items-center gap-2 [&_select]:h-8 [&_select]:max-w-56 [&_select]:min-w-0 [&_select]:rounded-md [&_select]:border [&_select]:border-line [&_select]:bg-surface [&_select]:px-2.5 [&_select]:text-caption [&_select]:text-fg-muted [&_select]:shadow-card [&_button]:flex-none">
        <select
          aria-label="Selecionar nó do grafo"
          value={selectedId && graph.nodes.some((node) => node.id === selectedId) ? selectedId : ""}
          onChange={(event) => onSelect(event.target.value || null)}
        >
          <option value="">Selecione um nó…</option>
          {graph.nodes.map((node) => (
            <option key={node.id} value={node.id}>
              {node.name} · {node.path || "Repository root"}
            </option>
          ))}
        </select>
        <DSButton onClick={fitToView}>
          <Maximize className="size-3.5" />
          Ajustar grafo
        </DSButton>
      </div>
      <div className="index-network-legend pointer-events-none absolute top-10 right-5 left-5 flex flex-wrap gap-x-3 gap-y-1.5 text-caption text-fg-muted">
        {legend.map((kind) => (
          <span className="graph-legend-item inline-flex items-center gap-1.5" key={kind}>
            <span
              className="graph-legend-dot inline-block size-2.5 rounded-full"
              style={{ background: `var(${kindColors[mode][kind] ?? "--text-2"})` }}
            />
            {kind}
          </span>
        ))}
      </div>
    </div>
  );
}

export function GraphEmpty({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="index-empty absolute inset-0 flex flex-col items-center justify-center gap-2 p-7 text-center [&_h3]:mt-2 [&_h3]:text-body [&_h3]:font-semibold [&_h3]:text-fg [&_p]:max-w-sm [&_p]:text-small [&_p]:text-fg-muted">
      <BrandMark size="md" />
      <h3>{title}</h3>
      <p>{description}</p>
      {children}
    </div>
  );
}
