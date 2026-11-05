import {
  Background,
  BackgroundVariant,
  Controls,
  type NodeChange,
  ReactFlow,
  type ReactFlowInstance,
} from "@xyflow/react";
import { X } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ContextPhaseDetails, contextGroupClass, FlowStateIcon, nodeStatus, nodeTypes } from "./ReviewFlowNodes";
import "@xyflow/react/dist/style.css";
import { useQuery } from "@tanstack/react-query";
import { BrandLoading } from "../../components/BrandLoading";
import { DSButton, DSError } from "../../components/DesignSystem";
import { setupApi } from "../../services/setupApi";
import type { ReviewPhase, ReviewRun, ReviewRunStage } from "../../types";
import {
  buildReviewChunks,
  buildReviewFlow,
  chunkCountsLabel,
  formatFlowDuration,
  formatFlowTokens,
  isReviewResultView,
  parseReviewPhases,
  phaseLabel,
  phaseMetrics,
  phaseStages,
  type ReviewChunks,
  type ReviewFlowControl,
  type ReviewFlowNode,
  type ReviewFlowNodeData,
  type ReviewResultSummary,
  stageTokens,
} from "../lib/reviewFlow";
import { required } from "../lib/utils";

export function ReviewFlow({
  phases,
  phaseJson,
  runStatus,
  run,
  stages = [],
  now = Date.now(),
  usageError,
  selectedView,
  onSelectView,
  reviewDetails,
  findingCount,
  control,
  result,
  resultAction,
  onFullReview,
}: {
  phases?: ReviewPhase[];
  phaseJson?: string | null;
  runStatus: string | null;
  run?: ReviewRun | null;
  stages?: ReviewRunStage[];
  now?: number;
  usageError?: string | null;
  selectedView: string | null;
  onSelectView: (view: string | null) => void;
  reviewDetails?: ReactNode;
  findingCount?: number;
  control?: ReviewFlowControl;
  result?: ReviewResultSummary;
  resultAction?: ReactNode;
  onFullReview?: () => void;
}) {
  const parsedPhases = useMemo(() => {
    const items = phases ?? parseReviewPhases(phaseJson);
    return items.some((phase) => phase.id === "ready")
      ? items
      : [...items, { id: "ready", label: "Review result", status: "pending" }];
  }, [phases, phaseJson]);
  const [columns, setColumns] = useState(3);
  const [measurements, setMeasurements] = useState<Record<string, { width: number; height: number }>>({});
  const onNodesChange = useCallback((changes: NodeChange<ReviewFlowNode>[]) => {
    setMeasurements((current) => {
      let next = current;
      for (const change of changes) {
        if (change.type !== "dimensions" || !change.dimensions) continue;
        const { width, height } = change.dimensions;
        if (current[change.id]?.width !== width || current[change.id]?.height !== height) {
          if (next === current) next = { ...current };
          next[change.id] = { width, height };
        }
      }
      return next;
    });
  }, []);
  const chunkCache = useRef<ReviewChunks | null>(null);
  const chunks = useMemo(
    () => buildReviewChunks(parsedPhases, run ?? null, stages, chunkCache.current, now),
    [parsedPhases, run, stages, now],
  );
  useEffect(() => {
    chunkCache.current = chunks;
  }, [chunks]);
  const flow = useMemo(
    () => buildReviewFlow(parsedPhases, runStatus, columns, chunks, control),
    [parsedPhases, runStatus, columns, chunks, control],
  );
  const selectedId = selectedView === "overview" ? "ready" : selectedView;
  const [instance, setInstance] = useState<ReactFlowInstance<ReviewFlowNode> | null>(null);
  const canvasRef = useRef<HTMLElement>(null);
  const inspectorRef = useRef<HTMLElement>(null);
  const topology = flow.nodes.map((node) => node.id).join(",");
  const closeInspector = useCallback(() => {
    const node = canvasRef.current?.querySelector<HTMLElement>(`[data-phase-id="${CSS.escape(selectedId ?? "")}"]`);
    onSelectView(null);
    (node ?? canvasRef.current?.querySelector<HTMLElement>('[data-phase-id="ready"]'))?.focus();
  }, [selectedId, onSelectView]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: topology and columns are triggers: refit the canvas when nodes or the column layout change
  useEffect(() => {
    if (!instance || !canvasRef.current) return;
    let frame = 0;
    const fitToView = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        void instance.fitView({ padding: 0.22, maxZoom: 1.1, includeHiddenNodes: true });
      });
    };
    const observer = new ResizeObserver(([entry]) => {
      setColumns(Math.max(1, Math.min(4, Math.floor((entry.contentRect.width - 80) / 246))));
      fitToView();
    });
    observer.observe(canvasRef.current);
    fitToView();
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [instance, topology, columns]);
  useEffect(() => {
    if (selectedId && !flow.nodes.some((node) => node.id === selectedId)) onSelectView("overview");
  }, [flow.nodes, selectedId, onSelectView]);
  useEffect(() => {
    if (selectedView) inspectorRef.current?.focus();
  }, [selectedView]);
  const nodes = useMemo(
    () =>
      flow.nodes.map((node) => ({
        ...node,
        measured: measurements[node.id],
        selected: node.id === selectedId,
        data: {
          ...node.data,
          ...(node.id === "ready" && result
            ? { result, state: result.state, lastCompleted: node.data.lastCompleted || result.state === "completed" }
            : {}),
          metrics: node.data.metrics ?? phaseMetrics(node.data.phase, node.data.state, stages, run ?? null, now),
          onSelect: () => onSelectView(node.id),
        },
      })),
    [flow.nodes, selectedId, stages, run, now, onSelectView, result, measurements],
  );
  const selected = nodes.find((node) => node.id === selectedId) ?? null;
  const phase = selected?.data.phase;
  const failed = selected && ["failed", "cancelled", "superseded"].includes(selected.data.state);
  const errorMessage = selected?.data.chunk?.error || run?.errorMessage;
  const canForceFullReview = Boolean(onFullReview && errorMessage?.includes("Choose Re-run full"));
  const log = selected?.data.chunk
    ? selected.data.chunk.log
    : typeof phase?.log === "string"
      ? phase.log
      : selected?.data.state === "running" || failed
        ? run?.tail
        : null;
  const startedAt = typeof phase?.startedAt === "number" ? phase.startedAt : null;
  const events = Array.isArray(phase?.events)
    ? phase.events.filter((event) => event && typeof event.at === "number" && typeof event.message === "string")
    : [];
  const executions = selectedId ? phaseStages(selectedId, stages) : [];
  const stageNote = selected?.data.chunk?.cached
    ? "Validated cache hit. No new model inference was charged for this chunk."
    : selected?.data.metrics.tokens === 0
      ? "Local processing. No Claude tokens are used by this stage."
      : "Tokens are recorded when each Claude execution finishes. Partial totals exclude executions still running; missing usage is not treated as zero.";
  const contextQuery = useQuery({
    queryKey: ["reviews", run?.prId, "runContext", run?.id, phase?.status],
    queryFn: () => {
      const current = required(run, "review run");
      return setupApi.getReviewRunContext(current.prId, current.id);
    },
    enabled: selectedId === "context" && Boolean(run),
    refetchInterval: selectedId === "context" && phase?.status === "running" ? 3000 : false,
  });
  return (
    <div className={`review-flow ${selectedView !== null ? "review-flow-inspecting" : ""}`}>
      <section ref={canvasRef} className="review-flow-canvas" aria-label="Interactive review process">
        <ReactFlow<ReviewFlowNode>
          nodes={nodes}
          edges={flow.edges}
          nodeTypes={nodeTypes}
          onInit={setInstance}
          onNodesChange={onNodesChange}
          colorMode="dark"
          nodesDraggable={false}
          nodesConnectable={false}
          nodesFocusable={false}
          edgesFocusable={false}
          autoPanOnNodeFocus={false}
          elementsSelectable
          elevateNodesOnSelect={false}
          onlyRenderVisibleElements
          panOnDrag
          zoomOnScroll
          zoomOnPinch
          fitView
          fitViewOptions={{ padding: 0.22, maxZoom: 1.1, includeHiddenNodes: true }}
          minZoom={0.25}
          maxZoom={1.8}
        >
          <Background variant={BackgroundVariant.Dots} gap={22} size={1} />
          <Controls showInteractive={false} />
        </ReactFlow>
        <div className="review-flow-hint">Select a stage to inspect its output</div>
      </section>
      {selectedView !== null && (
        <aside
          ref={inspectorRef}
          tabIndex={-1}
          className="review-stage-inspector z-5 flex min-h-0 w-[clamp(480px,48vw,800px)] min-w-0 flex-none flex-col border-l border-line bg-surface outline-none max-lg:absolute max-lg:inset-y-0 max-lg:right-0 max-lg:w-[min(620px,100%)] max-lg:shadow-pop"
          aria-label="Review details"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.stopPropagation();
              closeInspector();
            }
          }}
        >
          <header className="review-stage-header flex flex-none items-center justify-between border-b border-line px-4.5 py-2.5 [&_h2]:text-heading [&_h2]:text-fg">
            <h2>{selected ? phaseLabel(selected.data.phase) : "Review result"}</h2>
            <DSButton tone="ghost" aria-label="Fechar detalhes" onClick={closeInspector}>
              <X className="size-4" />
            </DSButton>
          </header>
          {isReviewResultView(selectedView) ? (
            <>
              {resultAction && (
                <div className="review-result-action flex justify-end border-b border-line px-5 py-2.5">
                  {resultAction}
                </div>
              )}
              {reviewDetails}
            </>
          ) : (
            selected && (
              <div className="review-stage-body flex min-h-0 flex-col items-start gap-3 overflow-auto p-4.5 [&_h3]:text-small [&_h3]:font-semibold [&_h3]:text-fg">
                <div className="flex w-full flex-wrap items-center gap-x-3 gap-y-1.5">
                  <span
                    className={`review-flow-status-pill inline-flex items-center gap-1.5 rounded-full px-2 py-1 text-caption font-medium review-flow-status-${selected.data.state}`}
                  >
                    <FlowStateIcon state={selected.data.state} skipped={nodeStatus(selected.data) === "Ignorado"} />
                    {nodeStatus(selected.data)}
                  </span>
                  <span className="review-stage-times text-caption text-fg-muted tabular-nums" title={stageNote}>
                    {[
                      formatFlowDuration(selected.data.metrics.durationMs),
                      // A stage that never called a model has no tokens to report; "0 tokens" there is noise.
                      executions.length > 0 || (selected.data.metrics.tokens ?? 0) > 0
                        ? `${formatFlowTokens(selected.data.metrics.tokens)}${selected.data.metrics.tokens !== null && selected.data.metrics.partial ? " (partial)" : ""}`
                        : null,
                      startedAt !== null ? `started ${new Date(startedAt).toLocaleTimeString()}` : null,
                      selected.data.chunk ? `queued ${formatFlowDuration(selected.data.chunk.queuedDurationMs)}` : null,
                      selected.data.chunk && selected.data.chunk.attempts > 1
                        ? `${selected.data.chunk.attempts} attempts`
                        : null,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                </div>
                {!(phase?.id === "context" && contextQuery.data) && (
                  <p className="review-stage-description text-small text-fg [overflow-wrap:anywhere]">
                    {selected.data.detail}
                  </p>
                )}
                {phase && ["merge", "final", "validate"].includes(phase.id) && (
                  <DSButton tone="primary" onClick={() => onSelectView("overview")}>
                    {findingCount !== undefined
                      ? `Review ${findingCount} finding${findingCount === 1 ? "" : "s"}`
                      : "Achados da revisão"}
                  </DSButton>
                )}
                {phase?.id === "chunks" && (
                  <p className="review-stage-note">
                    {chunkCountsLabel(chunks, Boolean(phase.inferenceSkipped))}
                    {chunks.counts.unknown > 0 && " · Counts exclude chunks whose status is unavailable."}
                  </p>
                )}
                {selected.data.chunk?.cached && <p className="review-stage-note">{stageNote}</p>}
                {usageError && <DSError>{usageError}</DSError>}
                {failed && errorMessage && (
                  <>
                    <div
                      className="review-stage-error w-full rounded-lg border border-rose-200 bg-rose-50 p-3 text-caption text-rose-800 [overflow-wrap:anywhere]"
                      role="alert"
                    >
                      {selected.data.chunk?.error ? "Chunk error: " : selected.data.chunk ? "Run error: " : ""}
                      {errorMessage}
                    </div>
                    {canForceFullReview && (
                      <DSButton
                        tone="primary"
                        disabled={control?.busy || control?.disabled}
                        onClick={() => onFullReview?.()}
                      >
                        Re-run full
                      </DSButton>
                    )}
                  </>
                )}
                {phase?.id === "context" && (
                  <>
                    {contextQuery.isLoading && <BrandLoading size="compact" label="Loading context snapshot…" />}
                    {contextQuery.error && <DSError>{contextQuery.error.message}</DSError>}
                    {contextQuery.data ? (
                      <ContextPhaseDetails context={contextQuery.data} />
                    ) : (
                      !contextQuery.isLoading &&
                      !contextQuery.error && (
                        <p className="review-stage-note">No context snapshot recorded for this run yet.</p>
                      )
                    )}
                  </>
                )}
                {executions.length > 0 && (
                  <details className={`review-stage-executions w-full ${contextGroupClass}`} open>
                    <summary>
                      Model runs<span>{executions.length}</span>
                    </summary>
                    <div className="mt-2 grid">
                      {executions.map((execution) => (
                        <div
                          key={execution.id}
                          className="review-stage-execution grid grid-cols-[minmax(0,1fr)_auto] items-start gap-2 border-b border-line py-2.5 text-caption [&_h4]:text-small [&_h4]:font-semibold [&_h4]:text-fg [&_p]:mt-1 [&_p]:text-fg-muted [&>span]:font-medium [&>span]:text-fg"
                        >
                          <div>
                            <h4>
                              {execution.chunkId !== null
                                ? `Chunk ${execution.chunkId}`
                                : phaseLabel(selected.data.phase)}
                            </h4>
                            <p>
                              {execution.fileCount} files · {execution.model ?? "Model unavailable"}
                            </p>
                            <p>
                              {execution.status} ·{" "}
                              {formatFlowDuration(
                                execution.finishedAt !== null ? execution.finishedAt - execution.createdAt : null,
                              )}
                            </p>
                          </div>
                          <span>{formatFlowTokens(stageTokens(execution))}</span>
                        </div>
                      ))}
                    </div>
                  </details>
                )}
                {events.length > 0 && (
                  // The timeline is what most stages have to show; Context has its files instead.
                  <StageTimeline events={events} state={selected.data.state} open={phase?.id !== "context"} />
                )}
                <StageOutput
                  log={log ?? ""}
                  empty={
                    selected.data.chunk
                      ? "No retained output for this chunk. The live feed only includes the chunk currently reported by the worker."
                      : selected.data.state === "pending"
                        ? "This stage has not started yet."
                        : "No stage output recorded for this run."
                  }
                />
              </div>
            )
          )}
        </aside>
      )}
    </div>
  );
}

/** A few lines of output read inline; a longer log folds away behind its line count. */
function StageOutput({ log, empty }: { log: string; empty: string }) {
  const lines = log ? log.split("\n").length : 0;
  const pre = (
    <pre className="review-stage-log max-h-[55dvh] w-full flex-none overflow-auto rounded-lg border border-line bg-canvas px-3 py-2 font-mono text-[0.7857rem] leading-relaxed font-normal whitespace-pre-wrap text-fg-muted [overflow-wrap:anywhere]">
      {log || empty}
    </pre>
  );
  if (lines > INLINE_OUTPUT_LINES)
    return (
      <details className={`review-stage-output w-full ${contextGroupClass}`}>
        <summary>
          Stage output<span>{lines} lines</span>
        </summary>
        {pre}
      </details>
    );
  return (
    <section className="review-stage-output grid w-full gap-1.5">
      <h3 className="text-caption font-semibold tracking-wide text-fg-subtle uppercase">Output</h3>
      {pre}
    </section>
  );
}

const INLINE_OUTPUT_LINES = 3;

function StageTimeline({
  events,
  state,
  open,
}: {
  events: Array<{ at: number; message: string }>;
  state: ReviewFlowNodeData["state"];
  open: boolean;
}) {
  const last = events.length - 1;
  const tone =
    state === "failed"
      ? "bg-danger ring-rose-100"
      : state === "running"
        ? "bg-brand ring-brand-soft animate-pulse"
        : "bg-success ring-emerald-100";
  return (
    <details className={`review-stage-history w-full ${contextGroupClass}`} open={open}>
      <summary>
        What happened
        <span>
          {events.length} {events.length === 1 ? "event" : "events"} ·{" "}
          {formatFlowDuration(events[last].at - events[0].at)}
        </span>
      </summary>
      <ol className="mt-3 max-h-90 overflow-auto pr-1">
        {events.map((event, index) => (
          <li
            // biome-ignore lint/suspicious/noArrayIndexKey: read-only list that is never reordered; entries can repeat, so the index keeps keys unique
            key={`${event.at}:${index}`}
            className="relative grid grid-cols-[1rem_minmax(0,1fr)] gap-x-2.5 pb-3 last:pb-0"
          >
            {index < last && (
              <span
                aria-hidden="true"
                className="absolute top-3 -bottom-1 left-[calc(0.5rem-1px)] w-0.5 rounded-full bg-line-strong"
              />
            )}
            <span
              aria-hidden="true"
              className={`relative mt-1 size-2.5 justify-self-center rounded-full ring-4 ${index === last ? tone : "border-2 border-line-strong bg-surface ring-surface"}`}
            />
            <div className="min-w-0">
              <div className="flex items-baseline gap-2 text-caption text-fg-subtle">
                <time>{new Date(event.at).toLocaleTimeString()}</time>
                {index > 0 && (
                  <span className="tabular-nums">+{formatFlowDuration(event.at - events[index - 1].at)}</span>
                )}
              </div>
              <p
                className={`mt-0.5 text-caption font-normal [overflow-wrap:anywhere] ${index === last ? "text-fg" : "text-fg-muted"}`}
              >
                {event.message}
              </p>
            </div>
          </li>
        ))}
      </ol>
    </details>
  );
}
