import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ChevronDown, Database, PanelLeftClose, PanelLeftOpen, RefreshCw, Search } from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { BrandLoading, BrandLoadingIcon } from "../../components/BrandLoading";
import { BrandMark } from "../../components/BrandMark";
import { DSButton, DSEmptyState, DSError, DSInput } from "../../components/DesignSystem";
import { type IndexJob, type IndexJobsResponse, setupApi } from "../../services/setupApi";
import { PlatformPage } from "../components/PlatformHeader";
import {
  indexGroups,
  indexingWorkspaceReducer,
  indexRevision,
  indexStateLabel,
  selectRepository,
} from "../lib/indexing";
import { formatError } from "../lib/utils";

const IndexGraphView = lazy(() =>
  import("../components/IndexGraph").then((module) => ({ default: module.IndexGraphView })),
);
const emptyJobs: IndexJob[] = [];
const wideScreen = () => typeof window !== "undefined" && window.matchMedia("(min-width: 801px)").matches;

export function IndexingPage() {
  const queryClient = useQueryClient();
  const [selectedRepoId, setSelectedRepoId] = useState<number | null>(null);
  const [search, setSearch] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [workspace, dispatch] = useReducer(indexingWorkspaceReducer, null, () => ({
    listOpen: wideScreen(),
    restoreList: wideScreen(),
    nodeId: null,
  }));
  const focusListToggle = useRef(false);
  const toggleList = () => {
    focusListToggle.current = true;
    dispatch({ type: "toggle-list" });
  };
  useEffect(() => {
    if (!focusListToggle.current) return;
    document.getElementById(workspace.listOpen ? "index-repositories-close" : "index-repositories-toggle")?.focus();
    focusListToggle.current = false;
  }, [workspace.listOpen]);
  const lastVersions = useRef<Record<number, string>>({});
  const [versions, setVersions] = useState<Record<number, string>>({});
  const jobsQuery = useQuery({
    queryKey: ["indexing", "jobs"],
    queryFn: setupApi.listIndexJobs,
    refetchInterval: 2000,
  });
  const [requests, setRequests] = useState<Record<number, { pending: boolean; error: string | null }>>({});
  const reindex = useMutation({
    mutationFn: setupApi.reindexRepo,
    onMutate: (repoId) => {
      setRequests((current) => ({ ...current, [repoId]: { pending: true, error: null } }));
    },
    onSuccess: (status, repoId) => {
      queryClient.setQueryData<IndexJobsResponse>(
        ["indexing", "jobs"],
        (current) =>
          current && {
            ...current,
            jobs: current.jobs.map((job) => (job.repo.id === repoId ? { ...job, status } : job)),
          },
      );
      setRequests((current) => ({ ...current, [repoId]: { pending: false, error: null } }));
      void queryClient.invalidateQueries({ queryKey: ["indexing", "jobs"] });
    },
    onError: (cause, repoId) => {
      setRequests((current) => ({ ...current, [repoId]: { pending: false, error: formatError(cause) } }));
    },
  });
  const [batchCompleted, setBatchCompleted] = useState(0);
  const reindexAll = useMutation({
    onMutate: () => setBatchCompleted(0),
    mutationFn: async (repoIds: number[]) => {
      let started = 0;
      for (const repoId of repoIds) {
        try {
          await reindex.mutateAsync(repoId);
          started++;
        } catch {
        } finally {
          setBatchCompleted((current) => current + 1);
        }
      }
      return { started };
    },
  });
  const batchFailures = (reindexAll.variables ?? []).filter((repoId) => requests[repoId]?.error).length;
  const jobs = jobsQuery.data?.jobs ?? emptyJobs;
  const eligibleIds = jobs
    .filter((job) => !["queued", "indexing"].includes(job.status.status) && !requests[job.repo.id]?.pending)
    .map((job) => job.repo.id);
  const selectedId = selectRepository(jobs, selectedRepoId);
  const selectedJob = jobs.find((job) => job.repo.id === selectedId) ?? null;
  const version = selectedId === null ? null : (versions[selectedId] ?? null);
  const selectNode = useCallback(
    (nodeId: string | null) => dispatch(nodeId === null ? { type: "close" } : { type: "inspect", nodeId }),
    [],
  );
  // biome-ignore lint/correctness/useExhaustiveDependencies: version is a trigger: reset the inspector when the selected repository gets a new index
  useEffect(() => {
    setSelectedRepoId(selectedId);
    dispatch({ type: "reset", wide: wideScreen() });
  }, [selectedId, version]);
  useEffect(() => {
    let changed = false;
    for (const job of jobs) {
      const revision = indexRevision(job.status);
      if (revision === null || revision === lastVersions.current[job.repo.id]) continue;
      if (lastVersions.current[job.repo.id])
        void queryClient.invalidateQueries({ queryKey: ["indexing", "graph", job.repo.id] });
      lastVersions.current[job.repo.id] = revision;
      changed = true;
    }
    if (changed) setVersions({ ...lastVersions.current });
  }, [jobs, queryClient]);
  useEffect(() => {
    const media = window.matchMedia("(min-width: 801px)");
    const resize = () => dispatch({ type: "resize", wide: media.matches });
    media.addEventListener("change", resize);
    return () => media.removeEventListener("change", resize);
  }, []);
  const counts = useMemo(() => {
    const totals: Record<string, number> = {};
    for (const job of jobs) totals[job.status.status] = (totals[job.status.status] ?? 0) + 1;
    return totals;
  }, [jobs]);
  const filtered = useMemo(
    () => jobs.filter((job) => job.repo.fullName.toLowerCase().includes(search.trim().toLowerCase())),
    [jobs, search],
  );
  const active = selectedJob && ["queued", "indexing"].includes(selectedJob.status.status);
  const requesting = selectedId !== null && requests[selectedId]?.pending;
  return (
    <PlatformPage
      title="Indexação"
      description="Explore o contexto de código por trás das revisões. Os repositórios selecionados ficam indexados automaticamente."
    >
      <div className="review-workspace-summary index-workspace-summary [&>span]:inline-flex [&>span]:items-center [&>span]:gap-1.5 mx-6 mb-3 flex flex-none flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border border-line bg-surface px-4 py-2.5 text-small text-fg-muted shadow-card">
        <span>
          <span className="index-status-dot inline-block size-1.5 flex-none rounded-full bg-current index-status-ready text-success" />
          {counts.ready ?? 0} ready
        </span>
        <span>{(counts.indexing ?? 0) + (counts.queued ?? 0)} in progress</span>
        <span>{(counts.failed ?? 0) + (counts.stale ?? 0) + (counts.cancelled ?? 0)} need attention</span>
        <span className="review-workspace-stream ml-auto text-caption text-fg-subtle">
          {jobsQuery.isError
            ? "Auto-refresh unavailable"
            : `Auto-refresh on${jobsQuery.dataUpdatedAt ? ` · checked ${new Date(jobsQuery.dataUpdatedAt).toLocaleTimeString()}` : ""}`}
        </span>
        <DSButton
          disabled={reindexAll.isPending || eligibleIds.length === 0}
          title="Reindexar todos os repositórios, pulando os que já estão na fila ou indexando"
          onClick={() => reindexAll.mutate(eligibleIds)}
        >
          {reindexAll.isPending ? <BrandLoadingIcon size={16} /> : <RefreshCw className="size-3.5" />}
          {reindexAll.isPending
            ? `Iniciando ${batchCompleted}/${reindexAll.variables?.length ?? 0}…`
            : "Reindexar todos"}
        </DSButton>
      </div>
      {!reindexAll.isPending && reindexAll.data && (
        <div className="index-batch-status flex-none px-6 pb-2.5 text-caption text-fg-subtle" role="status">
          Reindexação pedida para {reindexAll.data.started} repositórios.
          {batchFailures > 0 &&
            ` ${batchFailures} não começ${batchFailures === 1 ? "ou" : "aram"}. Selecione um repositório marcado para tentar de novo.`}
        </div>
      )}
      {jobsQuery.error && (
        <div className="index-page-error flex-none px-6 pb-3">
          <DSError>
            {formatError(jobsQuery.error)} <DSButton onClick={() => void jobsQuery.refetch()}>Tentar de novo</DSButton>
          </DSError>
        </div>
      )}
      <div
        className={`review-workspace index-workspace overflow-hidden flex min-h-0 flex-1 border-t border-line bg-canvas ${workspace.listOpen ? "review-workspace-list-open" : "review-workspace-list-collapsed"}`}
      >
        <aside
          id="index-repositories"
          hidden={!workspace.listOpen}
          className="review-workspace-list flex w-80 shrink-0 flex-col border-r border-line bg-surface [&[hidden]]:hidden"
          aria-label="Repositórios indexados"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              toggleList();
            }
          }}
        >
          <div className="review-workspace-list-header flex flex-none flex-col gap-3 border-b border-line p-4 [&_h2]:flex [&_h2]:items-center [&_h2]:gap-2 [&_h2]:text-overline [&_h2]:text-fg-subtle [&_h2]:">
            <div className="review-processes-heading flex items-center justify-between gap-2">
              <h2>
                Repositories <span>{jobs.length}</span>
              </h2>
              <DSButton
                id="index-repositories-close"
                tone="ghost"
                size="icon"
                className="review-processes-toggle"
                aria-label="Ocultar repositórios"
                title="Ocultar repositórios"
                aria-controls="index-repositories"
                aria-expanded={workspace.listOpen}
                onClick={toggleList}
              >
                <PanelLeftClose className="size-4" />
              </DSButton>
            </div>
            <DSInput
              aria-label="Buscar repositórios"
              icon={<Search className="size-4" />}
              value={search}
              placeholder="Buscar repositórios…"
              onChange={(event) => setSearch(event.target.value)}
            />
          </div>
          <div className="review-workspace-list-scroll flex min-h-0 flex-1 flex-col gap-1.5 overflow-y-auto p-3">
            {jobsQuery.isLoading ? (
              workspace.listOpen && <BrandLoading size="compact" label="Carregando repositórios…" />
            ) : filtered.length === 0 ? (
              <DSEmptyState>
                {search
                  ? "Nenhum repositório corresponde à busca."
                  : jobsQuery.isError
                    ? "Repositories could not be loaded. Retry above."
                    : "Selecione repositórios no setup para montar o índice."}
              </DSEmptyState>
            ) : (
              indexGroups.map((group) => {
                const items = filtered.filter((job) => group.states.includes(job.status.status));
                if (!items.length) return null;
                const closed = !search && collapsed[group.title];
                return (
                  <section key={group.title} className="index-repo-group mb-2.5 flex flex-col gap-0.5">
                    <button
                      type="button"
                      className="index-group-toggle flex w-full cursor-pointer items-center gap-2 px-1.5 py-2 text-left text-overline text-fg-subtle [&>span]:ml-auto [&>span]:tabular-nums"
                      aria-expanded={!closed}
                      onClick={() => setCollapsed((current) => ({ ...current, [group.title]: !closed }))}
                    >
                      <ChevronDown className={`size-3.5 ${closed ? "-rotate-90" : ""}`} />
                      {group.title}
                      <span>{items.length}</span>
                    </button>
                    {!closed &&
                      items.map((job) => (
                        <RepositoryRow
                          key={job.repo.id}
                          job={job}
                          request={requests[job.repo.id]}
                          selected={job.repo.id === selectedId}
                          onSelect={() => {
                            setSelectedRepoId(job.repo.id);
                            dispatch({ type: "reset", wide: wideScreen() });
                          }}
                        />
                      ))}
                  </section>
                );
              })
            )}
          </div>
        </aside>
        <div className="review-workspace-main flex min-w-0 flex-1 flex-col overflow-hidden">
          {(selectedJob || !workspace.listOpen) && (
            <header className="review-workspace-head index-workspace-head [&_h2]:flex [&_h2]:items-center [&_h2]:gap-2 [&_h2>svg]:flex-none [&_h2>svg]:text-fg-subtle flex flex-none items-center justify-between gap-4 border-b border-line bg-surface px-6 py-3.5 [&_h2]:text-heading [&_h2]:text-fg [&_p]:text-small [&_p]:text-fg-muted">
              <div className="flex min-w-0 items-center gap-3">
                {!workspace.listOpen && (
                  <DSButton
                    id="index-repositories-toggle"
                    tone="ghost"
                    size="icon"
                    className="review-processes-toggle"
                    aria-label="Mostrar repositórios"
                    title="Mostrar repositórios"
                    aria-controls="index-repositories"
                    aria-expanded={false}
                    onClick={toggleList}
                  >
                    <PanelLeftOpen className="size-4" />
                  </DSButton>
                )}
                {selectedJob && (
                  <div className="min-w-0">
                    <h2 title={selectedJob.repo.fullName}>
                      <Database className="size-4" />
                      {selectedJob.repo.fullName}
                    </h2>
                    <div className="index-repo-metadata mt-2 flex flex-wrap items-center gap-x-3 gap-y-2 text-caption text-fg-muted [&_code]:max-w-72 [&_code]:truncate [&_code]:rounded [&_code]:bg-surface-muted [&_code]:px-1.5 [&_code]:font-mono">
                      <span
                        className={`index-state inline-flex items-center gap-1.5 rounded-md bg-surface-muted px-2 py-0.5 text-caption font-medium index-status-${selectedJob.status.status}`}
                      >
                        {active ? (
                          <BrandLoadingIcon size={16} />
                        ) : selectedJob.status.status === "ready" ? (
                          <Check className="size-3" />
                        ) : (
                          <span className="index-status-dot inline-block size-1.5 flex-none rounded-full bg-current" />
                        )}
                        {indexStateLabel[selectedJob.status.status]}
                      </span>
                      {active && selectedJob.status.stage && (
                        <span>{selectedJob.status.stage.replaceAll("_", " ")}</span>
                      )}
                      {selectedJob.status.baseRef && <code>{selectedJob.status.baseRef}</code>}
                      {selectedJob.status.indexedAt && (
                        <span>Indexed {new Date(selectedJob.status.indexedAt).toLocaleString()}</span>
                      )}
                    </div>
                  </div>
                )}
              </div>
              {selectedJob && selectedJob.status.status !== "empty" && (
                <DSButton
                  tone="primary"
                  disabled={Boolean(active || requesting || reindexAll.isPending)}
                  onClick={() => reindex.mutate(selectedJob.repo.id)}
                >
                  {requesting ? <BrandLoadingIcon size={16} /> : <RefreshCw className="size-3.5" />}
                  {requesting
                    ? "Iniciando…"
                    : selectedJob.status.status === "failed"
                      ? "Tentar indexar de novo"
                      : "Reindexar"}
                </DSButton>
              )}
            </header>
          )}
          {selectedJob ? (
            <>
              {selectedJob.status.errorMessage && (
                <div
                  className="index-job-error max-h-32 flex-none overflow-auto border-b border-line bg-rose-50 px-6 py-2.5 text-caption text-rose-800 [overflow-wrap:anywhere] [&_button]:ml-2"
                  role="alert"
                >
                  {selectedJob.status.errorMessage}
                </div>
              )}
              {requests[selectedJob.repo.id]?.error && (
                <div className="index-page-error flex-none px-6 pb-3">
                  <DSError>{requests[selectedJob.repo.id].error}</DSError>
                </div>
              )}
              <Suspense fallback={<BrandLoading label="Carregando explorador de código…" />}>
                <IndexGraphView
                  key={`${selectedId}:${version ?? "initial"}`}
                  repoId={selectedJob.repo.id}
                  repoName={selectedJob.repo.fullName}
                  status={selectedJob.status}
                  selectedId={workspace.nodeId}
                  onSelect={selectNode}
                />
              </Suspense>
            </>
          ) : jobsQuery.isLoading ? (
            !workspace.listOpen && <BrandLoading label="Buscando status da indexação…" />
          ) : (
            <div className="index-empty flex h-full min-h-0 flex-1 flex-col items-center justify-center gap-3.5 overflow-auto p-7 text-center [&_h2]:text-heading [&_h2]:text-fg [&_h3]:text-heading [&_h3]:text-fg [&_p]:max-w-sm [&_p]:text-small [&_p]:text-fg-muted">
              <BrandMark size="lg" />
              <h2>{jobsQuery.isError ? "Unable to load repositories" : "Your code context starts here"}</h2>
              <p>
                {jobsQuery.isError
                  ? "Tente carregar os repositórios de novo pela ação acima."
                  : "Selecione repositórios no setup para explorar pacotes, arquivos e símbolos."}
              </p>
            </div>
          )}
        </div>
      </div>
    </PlatformPage>
  );
}

function RepositoryRow({
  job,
  request,
  selected,
  onSelect,
}: {
  job: IndexJob;
  request?: { pending: boolean; error: string | null };
  selected: boolean;
  onSelect: () => void;
}) {
  const separator = job.repo.fullName.lastIndexOf("/");
  const state = request?.pending ? "queued" : request?.error ? "failed" : job.status.status;
  const active = ["queued", "indexing"].includes(state);
  return (
    <button
      type="button"
      className={`index-repo-item grid w-full cursor-pointer grid-cols-[16px_minmax(0,1fr)_auto] items-center gap-2 rounded-md border border-transparent p-2 text-left text-fg transition-colors hover:bg-surface-muted ${selected ? "index-repo-item-selected border-brand/40 bg-brand-soft/60 ring-1 ring-brand/20" : ""}`}
      aria-pressed={selected}
      title={job.repo.fullName}
      onClick={onSelect}
    >
      <Database className="size-4 index-repo-icon mt-0.5 flex-none text-fg-subtle" />
      <span className="min-w-0">
        <span className="index-repo-name block truncate text-small font-semibold text-fg">
          {job.repo.fullName.slice(separator + 1)}
        </span>
        <span className="index-repo-owner mt-0.5 block truncate text-caption text-fg-subtle">
          {job.repo.fullName.slice(0, separator)}
        </span>
      </span>
      <span
        className={`index-repo-state inline-flex items-center gap-1 text-caption whitespace-nowrap index-status-${state}`}
      >
        {active ? (
          <BrandLoadingIcon size={16} />
        ) : (
          <span className="index-status-dot inline-block size-1.5 flex-none rounded-full bg-current" />
        )}
        {request?.error ? "Tentar de novo" : indexStateLabel[state]}
      </span>
    </button>
  );
}
