import { Handle, type NodeProps, type NodeTypes, Position, useUpdateNodeInternals } from "@xyflow/react";
import {
  AlertCircle,
  Bot,
  Brain,
  Check,
  ChevronRight,
  Clock,
  Database,
  FileCheck2,
  GitBranch,
  GitMerge,
  Layers2,
  ListChecks,
  type LucideIcon,
  Minus,
  Play,
  RotateCcw,
  SearchX,
  ShieldCheck,
  SkipForward,
  Square,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import { BrandLoadingIcon } from "../../components/BrandLoading";
import { DSBadge } from "../../components/DesignSystem";
import type { ReviewContextPlan } from "../../types";
import {
  chunkCountsLabel,
  flowStateLabel,
  formatFlowDuration,
  formatFlowTokens,
  phaseLabel,
  quoteShellArgument,
  type ReviewFlowNode,
  type ReviewFlowNodeData,
  type ReviewResultSummary,
} from "../lib/reviewFlow";

export const nodeTypes: NodeTypes = { reviewPhase: ReviewPhaseNode, reviewControl: ReviewControlNode };

export function nodeStatus(data: ReviewFlowNodeData) {
  if (data.phase.id === "chunks" && data.phase.inferenceSkipped && data.state === "failed") {
    return "Preparação falhou";
  }
  return (
    data.result?.status ??
    (data.chunk?.cached
      ? "Em cache"
      : data.chunk?.validating
        ? "Validando"
        : data.phase.status === "skipped"
          ? "Ignorado"
          : flowStateLabel(data.state))
  );
}

function ReviewControlNode({ data }: NodeProps<ReviewFlowNode>) {
  const control = data.control;
  if (!control) return null;
  const Icon = control.kind === "stop" ? Square : control.kind === "rerun" ? RotateCcw : Play;
  return (
    <div className={`review-flow-control review-flow-control-${control.kind}`}>
      <button
        type="button"
        className="nodrag nopan"
        aria-label={control.label}
        title={control.disabled && !control.busy ? `${control.label} unavailable for this MR` : control.label}
        disabled={control.disabled || control.busy}
        aria-busy={control.busy}
        onClick={(event) => {
          event.stopPropagation();
          control.onClick();
        }}
      >
        {control.busy ? <BrandLoadingIcon size={24} /> : <Icon aria-hidden="true" className="size-5" />}
      </button>
      <Handle type="source" position={Position.Right} className="review-flow-handle" />
    </div>
  );
}

const phaseIcons: Record<string, LucideIcon> = {
  index: Database,
  context: GitBranch,
  chunks: Layers2,
  merge: GitMerge,
  final: Brain,
  validate: ShieldCheck,
  review: Bot,
  ready: FileCheck2,
};

function ReviewStageIcon({
  phaseId,
  state,
  chunkId,
  result,
}: {
  phaseId: string;
  state: ReviewFlowNodeData["state"];
  chunkId?: number;
  result?: ReviewResultSummary;
}) {
  const isChunk = chunkId !== undefined;
  const isCleanResult =
    !isChunk && phaseId === "ready" && (result?.detail === "Nada a sinalizar" || result?.detail === "No findings");
  const Icon = isChunk ? Layers2 : isCleanResult ? SearchX : (phaseIcons[phaseId] ?? ListChecks);
  const iconKey = isChunk ? "chunk" : phaseId;
  return (
    <div
      className="review-flow-node-icon"
      data-review-stage-icon={iconKey}
      data-review-result-icon={isCleanResult ? "not-found" : undefined}
      aria-hidden="true"
    >
      {state === "running" ? <BrandLoadingIcon size={24} /> : <Icon className="size-4 review-flow-semantic-icon" />}
      {isChunk && (
        <span className="review-flow-chunk-number" data-review-chunk-number={chunkId}>
          {chunkId}
        </span>
      )}
    </div>
  );
}

function ReviewPhaseNode({ id, data, selected }: NodeProps<ReviewFlowNode>) {
  const updateNodeInternals = useUpdateNodeInternals();
  const group = Boolean(data.chunkGroup?.items.length);
  // biome-ignore lint/correctness/useExhaustiveDependencies: data.reverse and group change handle positions, so React Flow must re-measure the node
  useEffect(() => {
    updateNodeInternals(id);
  }, [id, data.reverse, group, updateNodeInternals]);
  const result = data.phase.id === "ready";
  const label = `${phaseLabel(data.phase)}: ${nodeStatus(data)}`;
  const content = (
    <>
      <ReviewStageIcon phaseId={data.phase.id} state={data.state} chunkId={data.chunk?.id} result={data.result} />
      <div className="min-w-0 review-flow-node-content">
        <div className="review-flow-node-label">
          {phaseLabel(data.phase)}
          {result && <ChevronRight className="size-3.5" aria-hidden="true" />}
        </div>
        <div className="review-flow-node-status">{nodeStatus(data)}</div>
        {result && (
          <div className="review-flow-result-detail">{data.result?.detail ?? "View findings and details"}</div>
        )}
        {!result && data.metrics && (
          <div className="review-flow-node-metrics">
            <span
              title={
                data.chunk
                  ? data.chunk.reportedTime
                    ? "Last duration reported by the worker"
                    : "Recorded Claude execution time"
                  : "Total stage time"
              }
            >
              <Clock className="size-3" />
              {formatFlowDuration(data.metrics.durationMs)}
            </span>
            <span
              title={
                data.metrics.partial
                  ? "Partial tokens: only completed Claude executions are included"
                  : "Recorded stage tokens"
              }
            >
              {data.metrics.tokens === null ? "— tokens" : formatFlowTokens(data.metrics.tokens)}
              {data.metrics.tokens !== null && data.metrics.partial ? "*" : ""}
            </span>
          </div>
        )}
        {data.chunkGroup && (
          // biome-ignore lint/a11y/useAriaPropsSupportedByRole: names the visible chunk tally for screen readers that announce labels on generic elements
          <div className="review-flow-chunk-counts" aria-label="Chunk progress">
            {chunkCountsLabel(data.chunkGroup, Boolean(data.phase.inferenceSkipped))}
          </div>
        )}
      </div>
    </>
  );
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: role is chosen at runtime; the button variant handles click and Enter/Space, the group variant has no handlers
    // biome-ignore lint/a11y/useAriaPropsSupportedByRole: role is chosen at runtime (button or group); both support aria-label, and aria-pressed is only set for the button
    <div
      className={`review-flow-node review-flow-node-${data.state} ${data.lastCompleted ? "review-flow-node-last-completed" : ""} ${result ? "review-flow-result" : ""} ${group ? "review-flow-chunk-group" : ""} ${data.chunk ? "review-flow-chunk-node" : ""} ${selected ? "review-flow-node-selected" : ""}`}
      role={group ? "group" : "button"}
      tabIndex={group ? undefined : 0}
      data-phase-id={group ? undefined : data.phase.id}
      aria-pressed={group ? undefined : selected}
      aria-label={group ? "Individual review chunks" : label}
      onKeyDown={(event) => {
        if (!group && (event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          data.onSelect?.();
        }
      }}
      onClick={group ? undefined : () => data.onSelect?.()}
    >
      {!data.chunk && (
        <Handle
          type="target"
          position={data.reverse ? Position.Right : Position.Left}
          style={group ? { top: 46 } : undefined}
          className="review-flow-handle"
        />
      )}
      {group ? (
        <button
          type="button"
          className="review-flow-chunk-heading"
          data-phase-id={data.phase.id}
          aria-label={label}
          aria-pressed={selected}
          onClick={() => data.onSelect?.()}
        >
          {content}
        </button>
      ) : (
        content
      )}
      {!data.chunk && !result && (
        <Handle
          type="source"
          position={data.reverse ? Position.Left : Position.Right}
          style={group ? { top: 46 } : undefined}
          className="review-flow-handle"
        />
      )}
    </div>
  );
}

export function ContextPhaseDetails({ context }: { context: ReviewContextPlan }) {
  const selection = context.scopeSelection;
  const baseSync = selection?.mode === "base_sync";
  const contentReuse = selection?.mode === "content_reuse";
  const scoped = baseSync || contentReuse || selection?.mode === "reprocess";
  const excluded = selection?.excludedFiles ?? [];
  const mode =
    selection?.mode === "reprocess"
      ? [
          "Explicit re-run",
          "Explicit re-run: fresh analysis of the previous scope against the current base, without cached model results.",
        ]
      : contentReuse
        ? [
            "Content-verified reuse",
            "Content-verified reuse: reviews changed or unproven files against the current base, independent of commit order.",
          ]
        : baseSync
          ? [
              "Base synchronization",
              "Base synchronization: reviews only affected files against the current PR base; unchanged files reuse verified coverage.",
            ]
          : context.incremental
            ? ["Incremental review", "Incremental review: selects the PR-owned changes since the previous review."]
            : ["Full review", "Full review: selects the PR diff against its immediate base."];
  const short = (sha?: string | null) => sha?.slice(0, 12) ?? "Unavailable";
  const groups: ContextFileGroup[] = [
    ...(excluded.length ? [{ id: "excluded", title: "Excluded", files: excluded }] : []),
    { id: "changed", title: "Changed", files: context.changedFiles.map((path) => ({ path })) },
    ...(selection && selection.reusedFiles.length > 0
      ? [
          {
            id: "reused",
            title: "Reused",
            files: selection.reusedFiles.map((path) => ({
              path,
              reason:
                selection.sourceRunId == null
                  ? "Verified persisted review coverage"
                  : `Verified coverage from run ${selection.sourceRunId}`,
            })),
          },
        ]
      : []),
    {
      id: "related",
      title: "Related",
      note: context.omittedCount > 0 ? `${context.omittedCount} more left out` : undefined,
      files: context.relatedFiles.map((file) => ({ path: file.path, reason: file.reason, source: file.source })),
    },
    { id: "tests", title: "Tests", files: context.testFiles.map((path) => ({ path })) },
    { id: "conventions", title: "Conventions", files: context.conventionFiles.map((path) => ({ path })) },
    {
      id: "packages",
      title: "Packages",
      note: context.dependentPackages.length ? `${context.dependentPackages.length} dependent` : undefined,
      files: [
        ...context.packages.map((item) => ({ path: item.name, reason: item.packagePath || "Repository root" })),
        ...context.dependentPackages.map((path) => ({ path, source: "Dependent package" })),
      ],
    },
  ];
  return (
    <section className="review-context-details grid w-full min-w-0 gap-3">
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
        <DSBadge tone="brand" title={mode[1]}>
          {mode[0]}
        </DSBadge>
        {selection && (
          <span className="text-caption text-fg-muted tabular-nums">{`${context.changedFiles.length} file${context.changedFiles.length === 1 ? "" : "s"} to review · ${selection.reusedFiles.length} with reused coverage${excluded.length ? ` · ${excluded.length} excluded, not reviewed` : ""} · ${selection.fullPrFiles.length} in the PR`}</span>
        )}
      </div>
      <ContextFileGroups groups={groups} />
      <div>
        <details className={`review-context-scope ${contextGroupClass}`}>
          <summary>
            Git scope
            <span>
              {context.mergeBaseSha?.slice(0, 7) ?? "?"}…{context.headSha?.slice(0, 7) ?? "?"}
            </span>
          </summary>
          <div className="mt-3 grid gap-3 font-normal">
            <p className="text-caption text-fg-muted">
              {mode[1]} Related files come from the repository index and import graph.
              {context.scopeReason && ` ${context.scopeReason.replace(/\.$/, "")}.`}
            </p>
            <dl className="grid grid-cols-3 gap-x-4 gap-y-2.5 text-caption [&_dt]:text-fg-subtle [&_dd]:mt-0.5 [&_dd]:truncate [&_dd]:font-mono [&_dd]:text-fg">
              <div className="col-span-3">
                <dt>Immediate base</dt>
                <dd title={context.baseRef ?? undefined}>{context.baseRef ?? "Unavailable for this historical run"}</dd>
              </div>
              <div>
                <dt>Base SHA</dt>
                <dd>{short(context.reviewedBaseSha)}</dd>
              </div>
              <div>
                <dt>Merge-base</dt>
                <dd>{short(context.mergeBaseSha)}</dd>
              </div>
              <div>
                <dt>Head</dt>
                <dd>{short(context.headSha)}</dd>
              </div>
              <div>
                <dt>Index SHA</dt>
                <dd>{short(context.baseSha)}</dd>
              </div>
              {context.priorSha && (
                <div>
                  <dt>Previous review</dt>
                  <dd>{short(context.priorSha)}</dd>
                </div>
              )}
            </dl>
            {context.range && (!scoped || context.changedFiles.length > 0) && (
              <code className="review-context-range block rounded-md bg-surface-muted p-2 font-mono text-caption text-fg-muted [overflow-wrap:anywhere]">
                git diff {quoteShellArgument(context.range)}
                {scoped && ` -- ${context.changedFiles.map(quoteShellArgument).join(" ")}`}
              </code>
            )}
          </div>
        </details>
      </div>
    </section>
  );
}

export const contextGroupClass =
  "disclosure min-w-0 [&_summary]:text-small [&_ul]:grid [&_ul]:max-h-65 [&_ul]:gap-2.5 [&_ul]:overflow-auto";

/** A repository path in small monospace: the folder dimmed, the file name readable. */
function FilePath({ path }: { path: string }) {
  const slash = path.lastIndexOf("/");
  return (
    <code className="block font-mono text-[0.75rem] leading-5">
      {slash >= 0 && <span className="text-fg-subtle">{path.slice(0, slash + 1)}</span>}
      <span className="text-fg-muted">{path.slice(slash + 1)}</span>
    </code>
  );
}

type ContextFileGroup = {
  id: string;
  title: string;
  files: Array<{ path: string; reason?: string; source?: string }>;
  note?: string;
};

/** One number per kind of context file; picking one lists its files underneath. */
function ContextFileGroups({ groups }: { groups: ContextFileGroup[] }) {
  const [activeId, setActiveId] = useState("changed");
  const active = groups.find((group) => group.id === activeId) ?? groups[0];
  return (
    <div className="review-context-files grid min-w-0 gap-2">
      <fieldset
        className="m-0 grid min-w-0 grid-cols-[repeat(auto-fit,minmax(5.5rem,1fr))] gap-1.5 border-0 p-0"
        aria-label="Context files"
      >
        {groups.map((group) => (
          <button
            key={group.id}
            type="button"
            className={`review-context-file-group min-w-0 cursor-pointer rounded-lg border px-2.5 py-1.5 text-left transition-colors ${group.id === active.id ? "border-brand bg-brand-soft" : "border-line bg-canvas hover:bg-surface-muted"}`}
            aria-pressed={group.id === active.id}
            onClick={() => setActiveId(group.id)}
          >
            <b className="block text-heading font-semibold text-fg tabular-nums">{group.files.length}</b>
            <span className="block truncate text-caption text-fg-muted">{group.title}</span>
          </button>
        ))}
      </fieldset>
      {active.note && <p className="text-caption text-fg-subtle">{active.note}</p>}
      {active.files.length ? (
        <ul className="review-context-file-list grid max-h-65 min-w-0 overflow-auto">
          {active.files.map((file, index) => (
            <li
              // biome-ignore lint/suspicious/noArrayIndexKey: read-only list that is never reordered; entries can repeat, so the index keeps keys unique
              key={`${file.path}:${index}`}
              className="flex min-w-0 items-baseline justify-between gap-3 border-b border-line/60 py-1.5 last:border-b-0 [overflow-wrap:anywhere]"
            >
              <FilePath path={file.path} />
              {(file.reason || file.source) && (
                <span className="flex-none text-right text-caption text-fg-subtle">
                  {[file.reason, file.source].filter(Boolean).join(" · ")}
                </span>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <p className="review-stage-note">None selected for this run.</p>
      )}
    </div>
  );
}

export function FlowStateIcon({ state, skipped = false }: { state: ReviewFlowNodeData["state"]; skipped?: boolean }) {
  if (state === "running") return <BrandLoadingIcon size={16} />;
  const Icon =
    skipped || state === "skipped"
      ? SkipForward
      : state === "completed"
        ? Check
        : state === "awaiting_action"
          ? AlertCircle
          : state === "failed"
            ? AlertCircle
            : state === "cancelled"
              ? X
              : state === "superseded"
                ? RotateCcw
                : state === "queued"
                  ? Clock
                  : Minus;
  return <Icon aria-hidden="true" className="size-4" />;
}
