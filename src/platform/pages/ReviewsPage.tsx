import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ListFilter, PanelLeftClose, PanelLeftOpen, RefreshCw, Search } from "lucide-react";
import {
  type ComponentProps,
  type Dispatch,
  lazy,
  type SetStateAction,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useState,
} from "react";
import { BrandLoading } from "../../components/BrandLoading";
import { cx, DSBadge, DSButton, DSEmptyState, DSError, DSInput } from "../../components/DesignSystem";
import { GitLabMark } from "../../components/GitLabMark";
import { setupApi } from "../../services/setupApi";
import type { FindingAuditView, ReviewCard } from "../../types";
import type { FindingSelection } from "../components/FindingSelection";
import { MultiSelect } from "../components/MultiSelect";
import { useResizableWidth } from "../hooks/useResizableWidth";
import { useReviewRunStream } from "../hooks/useReviewRunStream";
import {
  canDecideFindings,
  candidateKind,
  type FindingSelectionDraft,
  findingSelectionDirty,
  prepareEmptyFindingDecision,
  selectedFindingIds,
  storedPointCounts,
} from "../lib/findings";
import {
  formatFlowDuration,
  formatFlowTokens,
  isReviewResultView,
  reviewControl,
  reviewResultSummary,
  reviewWorkspaceReducer,
  runMetrics,
} from "../lib/reviewFlow";
import { formatError, required } from "../lib/utils";
import { Avatar } from "./reviews/Avatar";
import { ReviewDetail } from "./reviews/ReviewDetail";
import { type ReviewSection, ReviewSections } from "./reviews/ReviewSections";
import { parseComments, type ReviewDecision, reviewError, streamStatusLabel } from "./reviews/reviewHelpers";
import { useReviewSelection } from "./reviews/useReviewSelection";

const ReviewFlow = lazy(() => import("../components/ReviewFlow").then((module) => ({ default: module.ReviewFlow })));

// Fixed order, most urgent first, so a section never moves when another empties or fills.
const sections: ReviewSection[] = [
  {
    id: "needs_you",
    title: "Precisa de você",
    lanes: ["manual_required", "ready", "failed", "needs_setup"],
    defaultOpen: true,
    attention: true,
  },
  { id: "ai_working", title: "IA revisando", lanes: ["reviewing", "queued", "open"], defaultOpen: true },
  {
    id: "outposts",
    title: "Concluído",
    lanes: ["reviewed_cleanly", "auto_posted", "posted", "approved"],
    defaultOpen: false,
    history: true,
  },
  {
    id: "ignored",
    title: "Rascunhos e ignorados",
    lanes: ["draft", "auto_skipped", "skipped", "size_skipped"],
    defaultOpen: false,
    history: true,
  },
  { id: "archive", title: "Fechados e mesclados", lanes: ["closed", "merged"], defaultOpen: false, history: true },
];

export function ReviewsPage() {
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [findingDraft, setFindingDraft] = useState<FindingSelectionDraft | null>(null);
  const [findingError, setFindingError] = useState<{ runId: number; message: string } | null>(null);
  const [workspace, dispatchWorkspace] = useReducer(reviewWorkspaceReducer, null, () => {
    const wide = window.matchMedia("(min-width: 801px)").matches;
    return { view: null, processesOpen: wide, restoreProcesses: wide };
  });
  const [streamVersion, setStreamVersion] = useState(0);
  const [search, setSearch] = useState("");
  const listWidth = useResizableWidth("ocr.reviewsListWidth", { initial: 344, compact: 76, max: 560 });
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [selectedAuthors, setSelectedAuthors] = useState<Set<string>>(new Set());
  const [selectedRepositories, setSelectedRepositories] = useState<Set<string>>(new Set());
  const [now, setNow] = useState(Date.now());
  const boardQuery = useQuery({
    queryKey: ["reviews", "board"],
    queryFn: setupApi.listReviewBoard,
    refetchInterval: 5000,
  });
  const pendingQuery = useQuery({ queryKey: ["pending"], queryFn: setupApi.listPendingActions, refetchInterval: 5000 });
  const pollStatusQuery = useQuery({
    queryKey: ["reviews", "poll", "status"],
    queryFn: setupApi.pollStatus,
    refetchInterval: 5000,
  });
  const items = useMemo(() => (boardQuery.data ?? []).flatMap((group) => group.items), [boardQuery.data]);
  const { monitoringId, setMonitoringId, monitoring } = useReviewSelection(items);
  const viewing = isReviewResultView(workspace.view) ? monitoring : null;
  const streamPrId = monitoringId;
  const runQuery = useReviewRunStream(streamPrId, streamVersion);
  const usageQuery = useQuery({
    queryKey: ["reviews", viewing?.id, "usage"],
    queryFn: () => setupApi.getPrUsage(required(viewing, "merge request").id),
    enabled: Boolean(viewing),
  });
  const authQuery = useQuery({
    queryKey: ["auth", "gitlab", "status"],
    queryFn: setupApi.gitlabAuthStatus,
    refetchInterval: 30000,
  });
  const contextQuery = useQuery({
    queryKey: ["reviews", viewing?.id, "runContext", runQuery.data?.id, runQuery.data?.status],
    queryFn: () =>
      setupApi.getReviewRunContext(required(viewing, "merge request").id, required(runQuery.data, "review run").id),
    enabled: Boolean(viewing && runQuery.data),
    refetchInterval: viewing && ["running", "pending"].includes(runQuery.data?.status ?? "") ? 5000 : false,
  });
  const poll = useMutation({
    mutationFn: setupApi.pollReviews,
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["reviews"] });
      void queryClient.invalidateQueries({ queryKey: ["pending"] });
    },
    onError: (cause) => setError(reviewError(cause)),
  });
  const start = useMutation({
    mutationFn: ({ prId, forceFull }: { prId: number; forceFull?: boolean }) => setupApi.startReview(prId, forceFull),
    onSuccess: (_, { prId }) => {
      setMonitoringId(prId);
      setStreamVersion((version) => version + 1);
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["reviews"] });
      void queryClient.invalidateQueries({ queryKey: ["pending"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const cancel = useMutation({
    mutationFn: (prId: number) => setupApi.cancelReview(prId),
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["reviews"] });
      void queryClient.invalidateQueries({ queryKey: ["pending"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const decide = useMutation({
    mutationFn: async ({
      prId,
      decision,
      runId,
      selectionRevision,
      emptyAuditHead,
    }: {
      prId: number;
      decision: ReviewDecision;
      runId: number;
      selectionRevision?: number;
      emptyAuditHead?: string;
    }) => {
      let revision = selectionRevision;
      if (emptyAuditHead !== undefined) {
        const prepared = await prepareEmptyFindingDecision(
          prId,
          runId,
          emptyAuditHead,
          () => setupApi.getReviewFindingAudit(prId, runId),
          (currentRevision) => setupApi.saveReviewFindingSelection(prId, runId, currentRevision, []),
        );
        revision = prepared.revision;
        queryClient.setQueriesData<FindingAuditView>({ queryKey: ["reviews", prId, "findingAudit", runId] }, prepared);
      }
      return setupApi.decideReview(prId, decision, runId, revision);
    },
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["reviews"] });
      void queryClient.invalidateQueries({ queryKey: ["pending"] });
    },
    onError: (cause, { prId, runId }) => {
      setError(formatError(cause));
      void queryClient.invalidateQueries({ queryKey: ["reviews", prId, "findingAudit", runId] });
      void queryClient.invalidateQueries({ queryKey: ["reviews", prId, "run"] });
    },
  });
  const retryResult = useMutation({
    mutationFn: setupApi.retryReviewResult,
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["reviews"] });
      void queryClient.invalidateQueries({ queryKey: ["pending"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const selectDetail = useCallback(
    (view: string | null) => {
      dispatchWorkspace(view === null ? { type: "close" } : { type: "inspect", view });
      if (isReviewResultView(view) && monitoringId !== null)
        void setupApi
          .markPrReviewSeen(monitoringId)
          .then(() => queryClient.invalidateQueries({ queryKey: ["pending"] }))
          .catch(() => undefined);
    },
    [monitoringId, queryClient],
  );
  // biome-ignore lint/correctness/useExhaustiveDependencies: monitoringId and the run id are triggers: reset the workspace for another PR or run
  useEffect(() => {
    dispatchWorkspace({ type: "reset", wide: window.matchMedia("(min-width: 801px)").matches });
    setFindingDraft(null);
    setFindingError(null);
  }, [monitoringId, runQuery.data?.id]);
  useEffect(() => {
    const media = window.matchMedia("(min-width: 801px)");
    const resize = () => dispatchWorkspace({ type: "resize", wide: media.matches });
    media.addEventListener("change", resize);
    return () => media.removeEventListener("change", resize);
  }, []);
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const monitoredRun = monitoring?.lane === "queued" ? null : (runQuery.data ?? null);
  const stagesQuery = useQuery({
    queryKey: ["reviews", monitoringId, "stages", monitoredRun?.id, monitoredRun?.status],
    queryFn: () =>
      setupApi.getReviewStageBreakdown(
        required(monitoringId, "merge request id"),
        required(monitoredRun, "review run").id,
      ),
    enabled: Boolean(monitoringId && monitoredRun),
    refetchInterval:
      monitoredRun &&
      (["running", "pending"].includes(monitoredRun.status) ||
        (monitoredRun.finishedAt !== null && now - monitoredRun.finishedAt < 15000))
        ? 3000
        : false,
  });
  const findingQuery = useQuery({
    queryKey: ["reviews", monitoringId, "findingAudit", monitoredRun?.id, monitoredRun?.status],
    queryFn: () =>
      setupApi.getReviewFindingAudit(
        required(monitoringId, "merge request id"),
        required(monitoredRun, "review run").id,
      ),
    enabled: Boolean(monitoringId && monitoredRun && workspace.view !== null),
    refetchInterval: monitoredRun && ["running", "pending"].includes(monitoredRun.status) ? 3000 : false,
  });
  const findingView = findingQuery.data?.runId === monitoredRun?.id ? findingQuery.data : undefined;
  const currentDraft =
    findingDraft?.runId === monitoredRun?.id && findingDraft?.prId === monitoringId ? findingDraft : null;
  const findingIds = currentDraft?.selectedIds ?? (findingView ? selectedFindingIds(findingView) : []);
  const findingDirty = findingView ? findingSelectionDirty(findingView, currentDraft) : false;
  const saveFindings = useMutation({
    mutationFn: (draft: FindingSelectionDraft) =>
      setupApi.saveReviewFindingSelection(draft.prId, draft.runId, draft.revision, draft.selectedIds),
    onSuccess: async (view, draft) => {
      queryClient.setQueriesData<FindingAuditView>(
        { queryKey: ["reviews", draft.prId, "findingAudit", draft.runId] },
        view,
      );
      await queryClient.invalidateQueries({ queryKey: ["reviews"] });
      setFindingDraft((current) => (current?.runId === draft.runId ? null : current));
      setFindingError((current) => (current?.runId === draft.runId ? null : current));
    },
    onError: (cause, draft) => {
      setFindingError({ runId: draft.runId, message: formatError(cause) });
      void queryClient.invalidateQueries({ queryKey: ["reviews", draft.prId, "findingAudit", draft.runId] });
    },
  });
  const findingConflict = Boolean(
    !saveFindings.isPending && currentDraft && findingView && currentDraft.revision !== findingView.revision,
  );
  const findingActionsReady =
    !findingQuery.isError && !saveFindings.isPending && canDecideFindings(findingView, currentDraft);
  const updateFindingIds = (selectedIds: string[]) => {
    if (!findingView?.editable || findingConflict || saveFindings.isPending) return;
    setFindingError(null);
    setFindingDraft({
      prId: findingView.prId,
      runId: findingView.runId,
      revision: currentDraft?.revision ?? findingView.revision,
      selectedIds,
    });
  };
  const findingSelection: ComponentProps<typeof FindingSelection> = {
    view: findingView,
    selectedIds: findingIds,
    loading: findingQuery.isLoading,
    busy: saveFindings.isPending || decide.isPending,
    dirty: findingDirty,
    conflict: findingConflict,
    error: findingQuery.error
      ? formatError(findingQuery.error)
      : findingError?.runId === monitoredRun?.id
        ? (findingError?.message ?? null)
        : null,
    onToggle: (id) => {
      if (findingView?.audit?.candidates.some((candidate) => candidate.id === id && !candidate.blockedReason))
        updateFindingIds(
          findingIds.includes(id) ? findingIds.filter((selected) => selected !== id) : [...findingIds, id],
        );
    },
    onSave: () => {
      if (findingView?.editable && !findingConflict)
        saveFindings.mutate({
          prId: findingView.prId,
          runId: findingView.runId,
          revision: currentDraft?.revision ?? findingView.revision,
          selectedIds: findingIds,
        });
    },
    onReset: () => {
      if (findingConflict) {
        setFindingDraft(null);
        setFindingError(null);
        void findingQuery.refetch();
      } else if (findingView?.audit)
        updateFindingIds(
          findingView.audit.candidates
            .filter((candidate) => candidate.defaultSelected && !candidate.blockedReason)
            .map((candidate) => candidate.id),
        );
    },
  };
  const stages = stagesQuery.data?.runId === monitoredRun?.id ? (stagesQuery.data?.stages ?? []) : [];
  const totals = runMetrics(monitoredRun, stages, now);
  const executionControl = monitoring ? reviewControl(monitoring, monitoredRun) : null;
  const canRetryPublication =
    monitoring?.resultPublicationStatus === "failed" &&
    monitoring.resultObservedHeadSha === monitoring.headSha &&
    executionControl?.kind !== "stop";
  // Without the audit (no stage open), count the run's stored comments and body.
  const stored = monitoredRun?.commentsJson
    ? storedPointCounts(
        parseComments(monitoredRun.commentsJson).map((comment) => comment.body),
        monitoredRun.reviewBody,
      )
    : undefined;
  const countKept = (kind: "finding" | "question") =>
    findingView?.audit?.candidates.filter(
      (candidate) => candidateKind(candidate) === kind && candidate.aiDecision === "kept" && !candidate.blockedReason,
    ).length ?? stored?.[kind === "finding" ? "findings" : "questions"];
  const resultFindingCount = countKept("finding");
  const resultQuestionCount = countKept("question") ?? 0;
  const authorOptions = useMemo(() => {
    const authors = new Map<string, ReviewCard>();
    for (const card of items)
      if (card.author && !authors.get(card.author)?.authorAvatarUrl) authors.set(card.author, card);
    return [...authors.values()]
      .sort((a, b) => a.author.localeCompare(b.author))
      .map((card) => ({ value: card.author, label: card.author, icon: <Avatar card={card} /> }));
  }, [items]);
  const repositoryOptions = useMemo(
    () =>
      [...new Set(items.map((item) => item.repoFullName).filter(Boolean))]
        .sort((a, b) => a.localeCompare(b))
        .map((value) => ({ value, label: value })),
    [items],
  );
  const query = search.trim().toLowerCase();
  const filtersActive = Boolean(query || selectedAuthors.size || selectedRepositories.size);
  const filterCount = selectedAuthors.size + selectedRepositories.size;
  const filteredItems = useMemo(
    () =>
      items.filter((item) => {
        const matchesSearch =
          !query ||
          [item.repoFullName, item.title, item.author, String(item.number), `#${item.number}`].some((value) =>
            value.toLowerCase().includes(query),
          );
        const matchesAuthor = !selectedAuthors.size || selectedAuthors.has(item.author);
        const matchesRepository = !selectedRepositories.size || selectedRepositories.has(item.repoFullName);
        return matchesSearch && matchesAuthor && matchesRepository;
      }),
    [items, query, selectedAuthors, selectedRepositories],
  );
  const toggleFilter = (setter: Dispatch<SetStateAction<Set<string>>>, value: string) =>
    setter((current) => {
      const next = new Set(current);
      if (next.has(value)) next.delete(value);
      else next.add(value);
      return next;
    });
  const clearFilters = () => {
    setSearch("");
    setSelectedAuthors(new Set());
    setSelectedRepositories(new Set());
  };
  const attentionPrIds = useMemo(
    () =>
      new Set(
        (pendingQuery.data ?? []).flatMap((item) =>
          item.status === "unseen" &&
          item.prId &&
          [
            "review_ready",
            "review_failed",
            "review_result_failed",
            "review_requests_restore_failed",
            "manual_review_needed",
          ].includes(item.kind)
            ? [item.prId]
            : [],
        ),
      ),
    [pendingQuery.data],
  );
  const nextPollAt =
    pollStatusQuery.data?.nextPollAt ??
    (pollStatusQuery.data?.lastPollAt
      ? pollStatusQuery.data.lastPollAt + pollStatusQuery.data.pollIntervalSeconds * 1000
      : null);
  const nextPollSeconds = nextPollAt ? Math.ceil((nextPollAt - now) / 1000) : null;
  const nextPollLabel =
    pollStatusQuery.data?.running || poll.isPending
      ? "Consultando agora"
      : nextPollSeconds === null
        ? "Próxima consulta pendente"
        : nextPollSeconds <= 0
          ? "Pronto para consultar"
          : `Next poll in ${nextPollSeconds}s`;
  const streamLabel = streamStatusLabel(runQuery.streamState);
  return (
    <>
      {error && (
        <div className="px-4 pt-3">
          <DSError>{error}</DSError>
        </div>
      )}
      {boardQuery.error && (
        <div className="px-4 pt-3">
          <DSError>{formatError(boardQuery.error)}</DSError>
        </div>
      )}
      <div
        className={`review-workspace review-workspace-reviews flex min-h-0 flex-1 bg-canvas ${workspace.processesOpen ? "review-workspace-list-open" : "review-workspace-list-collapsed"}`}
      >
        <aside
          id="review-processes"
          hidden={!workspace.processesOpen}
          className={cx(
            "review-workspace-list relative flex shrink-0 flex-col border-r border-line bg-surface [&[hidden]]:hidden",
            listWidth.dragging ? "select-none" : "transition-[width] duration-150",
          )}
          style={{ width: listWidth.width }}
          aria-label="Processos de revisão"
        >
          <div
            className="absolute top-0 -right-1 z-10 h-full w-2 cursor-col-resize transition-colors hover:bg-brand/30"
            title="Arraste para redimensionar · clique duplo para resetar"
            {...listWidth.handleProps}
          />
          <div className="review-workspace-list-header flex flex-none flex-col gap-2.5 px-3 pt-3 pb-2">
            <div
              className={cx(
                "review-processes-heading flex items-center gap-0.5",
                listWidth.compact && "justify-center",
              )}
            >
              <h2 className="mr-auto flex items-baseline gap-2 px-1.5 text-heading text-fg" hidden={listWidth.compact}>
                Reviews{" "}
                <span className="text-caption font-normal text-fg-subtle tabular-nums">
                  {filtersActive ? `${filteredItems.length}/${items.length}` : items.length}
                </span>
              </h2>
              <DSButton
                tone="ghost"
                size="icon"
                hidden={listWidth.compact}
                aria-label="Consultar agora"
                title={nextPollLabel}
                onClick={() => poll.mutate()}
                disabled={poll.isPending || pollStatusQuery.data?.running}
              >
                <RefreshCw
                  className={cx("size-4", (poll.isPending || pollStatusQuery.data?.running) && "animate-spin")}
                />
              </DSButton>
              <DSButton
                tone="ghost"
                size="icon"
                hidden={listWidth.compact}
                className={cx("relative", (filtersOpen || filterCount > 0) && "bg-surface-muted text-fg")}
                aria-label="Filtrar revisões"
                title="Filtrar por pessoas ou repositório"
                aria-expanded={filtersOpen}
                aria-controls="review-process-filters"
                onClick={() => setFiltersOpen((open) => !open)}
              >
                <ListFilter className="size-4" />
                {filterCount > 0 && (
                  <span className="absolute -top-0.5 -right-0.5 grid size-4 place-items-center rounded-full bg-brand text-[0.6429rem] font-semibold text-white tabular-nums">
                    {filterCount}
                  </span>
                )}
              </DSButton>
              <DSButton
                tone="ghost"
                size="icon"
                className="review-processes-toggle"
                aria-label="Ocultar processos"
                title="Ocultar processos"
                aria-controls="review-processes"
                aria-expanded={workspace.processesOpen}
                onClick={() => dispatchWorkspace({ type: "toggle-processes" })}
              >
                <PanelLeftClose className="size-4" />
              </DSButton>
            </div>
            {!listWidth.compact && (
              <DSInput
                aria-label="Buscar revisões"
                icon={<Search className="size-4" />}
                value={search}
                placeholder="Buscar MRs, repos, autores…"
                onChange={(event) => setSearch(event.target.value)}
              />
            )}
            {!listWidth.compact && (filtersOpen || filterCount > 0) && (
              <div id="review-process-filters" className="review-process-filters flex flex-col gap-2">
                <MultiSelect
                  label="People"
                  placeholder="Filtrar pessoas"
                  options={authorOptions}
                  selected={selectedAuthors}
                  loading={boardQuery.isLoading}
                  onToggle={(value) => toggleFilter(setSelectedAuthors, value)}
                />
                <MultiSelect
                  label="Repositórios"
                  placeholder="Filtrar repositórios"
                  presentation="dialog"
                  options={repositoryOptions}
                  selected={selectedRepositories}
                  loading={boardQuery.isLoading}
                  onToggle={(value) => toggleFilter(setSelectedRepositories, value)}
                />
                {filtersActive && (
                  <DSButton
                    tone="link"
                    className="review-process-filters-clear self-start"
                    aria-label="Limpar filtros de revisão"
                    title="Limpar todos os filtros"
                    onClick={clearFilters}
                  >
                    Clear filters
                  </DSButton>
                )}
              </div>
            )}
          </div>
          <div className="review-workspace-list-scroll min-h-0 flex-1 overflow-y-auto px-2 pb-3">
            {boardQuery.isLoading ? (
              workspace.processesOpen && <BrandLoading size="compact" label="Carregando revisões…" />
            ) : filteredItems.length === 0 ? (
              <DSEmptyState>
                {query
                  ? "Nenhum merge request corresponde à busca."
                  : "Consulte o GitLab para encontrar merge requests."}
              </DSEmptyState>
            ) : (
              <ReviewSections
                compact={listWidth.compact}
                sections={sections}
                items={filteredItems}
                expandAll={filtersActive}
                selectedId={monitoringId}
                attentionPrIds={attentionPrIds}
                now={now}
                onSelect={(card) => {
                  setMonitoringId(card.id);
                  dispatchWorkspace({
                    type: "reset",
                    wide: window.matchMedia("(min-width: 801px)").matches,
                  });
                }}
              />
            )}
          </div>
          <footer className="review-workspace-list-footer flex flex-none items-center gap-2 border-t border-line px-4 py-2.5 text-caption text-fg-subtle">
            <span
              className={cx("size-1.5 rounded-full", pollStatusQuery.data?.lastError ? "bg-danger" : "bg-success")}
            />
            {!listWidth.compact && (
              <>
                <span className="truncate tabular-nums">
                  {pollStatusQuery.data?.lastError ? "Consulta falhou · tentando de novo" : nextPollLabel}
                </span>
                {streamPrId && <span className="ml-auto truncate">{streamLabel}</span>}
              </>
            )}
          </footer>
        </aside>
        <div className="review-workspace-main flex min-w-0 flex-1 flex-col overflow-hidden">
          {(monitoring || !workspace.processesOpen) && (
            <header className="review-workspace-head flex flex-none items-center justify-between gap-4 border-b border-line bg-surface px-6 py-3.5">
              <div className="flex min-w-0 items-center gap-3">
                {!workspace.processesOpen && (
                  <DSButton
                    tone="ghost"
                    size="icon"
                    className="review-processes-toggle"
                    aria-label="Mostrar processos"
                    title="Mostrar processos"
                    aria-controls="review-processes"
                    aria-expanded={false}
                    onClick={() => dispatchWorkspace({ type: "toggle-processes" })}
                  >
                    <PanelLeftOpen className="size-4" />
                  </DSButton>
                )}
                {monitoring && (
                  <div className="min-w-0">
                    <div className="review-live-title-row flex items-center gap-2">
                      <Avatar card={monitoring} />
                      <h2 className="truncate text-caption font-medium text-fg-subtle">
                        {monitoring.repoFullName} <span className="tabular-nums">#{monitoring.number}</span>
                      </h2>
                    </div>
                    <p className="mt-0.5 truncate text-heading text-fg" title={monitoring.title}>
                      {monitoring.title}
                    </p>
                    {monitoredRun && (
                      <div className="review-run-metrics mt-1.5 flex flex-wrap gap-1.5">
                        {monitoredRun.model && (
                          <DSBadge title="Provedor, modelo e esforço">{monitoredRun.model}</DSBadge>
                        )}
                        <DSBadge>
                          Total time <b className="font-semibold text-fg">{formatFlowDuration(totals.durationMs)}</b>
                        </DSBadge>
                        <DSBadge
                          tone="violet"
                          title="Input, output and cache writes. Cache reads are shown in the run details. Running executions report usage when they finish."
                        >
                          Total recorded <b className="font-semibold">{formatFlowTokens(totals.tokens)}</b>
                          {totals.tokens !== null && totals.partial ? " · partial" : ""}
                        </DSBadge>
                      </div>
                    )}
                  </div>
                )}
              </div>
              {monitoring?.url && (
                <a
                  className="review-workspace-gitlab-link grid size-8 shrink-0 place-items-center rounded-lg text-fg-muted no-underline transition-colors hover:bg-surface-muted hover:text-fg"
                  href={monitoring.url}
                  target="_blank"
                  rel="noreferrer"
                  aria-label="Abrir no GitLab"
                  title="Abrir no GitLab"
                >
                  <GitLabMark className="size-4" />
                </a>
              )}
            </header>
          )}
          {monitoring ? (
            <>
              {(pendingQuery.data ?? [])
                .filter(
                  (item) =>
                    item.prId === monitoring.id &&
                    item.kind === "review_requests_restore_failed" &&
                    item.status !== "resolved",
                )
                .map((item) => (
                  <div
                    key={item.id}
                    className="review-stream-warning mx-6 mt-3 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-small text-amber-900"
                    role="alert"
                  >
                    {item.message}
                  </div>
                ))}
              {runQuery.streamError && (
                <div className="review-stream-warning mx-6 mt-3 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-small text-amber-900">
                  {runQuery.streamError} · retrying live updates
                </div>
              )}
              {runQuery.error && <DSError>{formatError(runQuery.error)}</DSError>}
              <Suspense fallback={<BrandLoading label="Carregando canvas do processo…" />}>
                <ReviewFlow
                  key={`${monitoring.id}:${monitoredRun?.id ?? "new"}`}
                  phaseJson={monitoredRun?.phaseJson}
                  runStatus={monitoredRun?.status ?? monitoring.lane}
                  run={monitoredRun}
                  stages={stages}
                  now={now}
                  usageError={stagesQuery.error ? formatError(stagesQuery.error) : null}
                  selectedView={workspace.view}
                  onSelectView={selectDetail}
                  findingCount={resultFindingCount}
                  onFullReview={() => start.mutate({ prId: monitoring.id, forceFull: true })}
                  control={
                    executionControl
                      ? {
                          ...executionControl,
                          disabled:
                            executionControl.disabled ||
                            runQuery.isLoading ||
                            retryResult.isPending ||
                            decide.isPending,
                          busy: start.isPending || cancel.isPending,
                          onClick: () =>
                            executionControl.kind === "stop"
                              ? cancel.mutate(monitoring.id)
                              : start.mutate({ prId: monitoring.id, forceFull: false }),
                        }
                      : undefined
                  }
                  result={reviewResultSummary(monitoring, monitoredRun, resultFindingCount, resultQuestionCount)}
                  resultAction={
                    canRetryPublication && (
                      <DSButton
                        tone="primary"
                        disabled={retryResult.isPending || start.isPending}
                        onClick={() => retryResult.mutate(monitoring.id)}
                      >
                        <RefreshCw className="size-3.5" />
                        {retryResult.isPending ? "Tentando publicar de novo…" : "Tentar publicar de novo"}
                      </DSButton>
                    )
                  }
                  reviewDetails={
                    viewing && (
                      <ReviewDetail
                        key={runQuery.data?.id ?? "new"}
                        card={viewing}
                        run={runQuery.data ?? null}
                        viewerLogin={authQuery.data?.login ?? null}
                        usage={usageQuery.data ?? null}
                        context={contextQuery.data ?? null}
                        contextError={contextQuery.error ? formatError(contextQuery.error) : null}
                        loading={runQuery.isLoading}
                        streamState={runQuery.streamState}
                        streamError={runQuery.streamError}
                        lastUpdatedAt={runQuery.lastUpdatedAt}
                        busy={start.isPending || cancel.isPending || decide.isPending || retryResult.isPending}
                        onReprocess={(forceFull) => start.mutate({ prId: viewing.id, forceFull })}
                        onCancel={() => cancel.mutate(viewing.id)}
                        findingSelectionReady={findingActionsReady}
                        findingSelection={findingSelection}
                        onDecide={(decision) => {
                          if (runQuery.data && findingActionsReady)
                            decide.mutate({
                              prId: viewing.id,
                              decision,
                              runId: runQuery.data.id,
                              selectionRevision: findingView?.requiresConfirmation ? findingView.revision : undefined,
                              emptyAuditHead:
                                findingView?.requiresConfirmation &&
                                findingView.audit?.complete &&
                                findingView.audit.candidates.length === 0
                                  ? findingView.audit.headSha
                                  : undefined,
                            });
                        }}
                      />
                    )
                  }
                />
              </Suspense>
            </>
          ) : boardQuery.isLoading ? (
            !workspace.processesOpen && <BrandLoading label="Carregando revisões…" />
          ) : (
            <div className="review-workspace-empty m-6">
              <DSEmptyState>Poll GitLab, then select a merge request to inspect its process.</DSEmptyState>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
