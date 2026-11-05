import type { Edge, Node } from "@xyflow/react";
import type { ReviewCard, ReviewChunkProgress, ReviewPhase, ReviewRun, ReviewRunStage } from "../../types";

export function quoteShellArgument(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export interface ReviewFlowControl {
  kind: "run" | "rerun" | "stop";
  label: string;
  disabled: boolean;
  busy?: boolean;
  onClick: () => void;
}

export function reviewNeedsAttention(card: Pick<ReviewCard, "lane"> & { state?: string }) {
  if (["closed", "merged"].includes(card.state ?? "")) return false;
  return ["manual_required", "ready", "failed", "needs_setup"].includes(card.lane);
}

export function reviewControl(card: ReviewCard, run: ReviewRun | null): Omit<ReviewFlowControl, "onClick"> {
  const active = ["reviewing", "queued"].includes(card.lane) || ["running", "pending"].includes(run?.status ?? "");
  return {
    kind: active ? "stop" : run ? "rerun" : "run",
    label: active ? "Parar revisão" : run ? "Reexecutar revisão" : "Executar revisão",
    disabled:
      !active &&
      (["closed", "merged"].includes(card.state) ||
        ["closed", "merged", "draft", "auto_skipped", "size_skipped"].includes(card.lane)),
  };
}

export interface ReviewResultSummary {
  status: string;
  detail: string;
  state: ReviewFlowState;
}

export function reviewResultSummary(
  card: ReviewCard,
  run: ReviewRun | null,
  findingCount?: number,
  questionCount = 0,
): ReviewResultSummary {
  if (card.lane === "size_skipped")
    return {
      status: "Ignorado",
      detail: card.statusReason ?? "O MR excede o limite de arquivos configurado",
      state: "completed",
    };
  if (card.lane === "queued" || run?.status === "pending")
    return { status: "Na fila", detail: "Pronto quando a revisão terminar", state: "pending" };
  if (card.lane === "reviewing" || run?.status === "running")
    return { status: "Aguardando revisão", detail: "Pronto quando a revisão terminar", state: "pending" };
  if (run && TERMINAL_FAILURES.has(run.status))
    return {
      status:
        run.status === "failed"
          ? "Revisão falhou"
          : run.status === "cancelled"
            ? "Revisão interrompida"
            : "Resultado desatualizado",
      detail: "Ver detalhes da execução",
      state: run.status as ReviewFlowState,
    };
  const current = card.resultObservedHeadSha === card.headSha;
  const publication = current ? card.resultPublicationStatus : null;
  const result = current ? card.resultState : null;
  if (run && run.headSha !== card.headSha && !result)
    return {
      status: "Resultado desatualizado",
      detail: "Reexecute para as alterações mais recentes",
      state: "superseded",
    };
  const noFindings = findingCount === 0 && questionCount === 0;
  const achados = (n: number) => `${n} achado${n === 1 ? "" : "s"}`;
  const perguntas = (n: number) => `${n} pergunta${n === 1 ? "" : "s"}`;
  const detail =
    card.suggestionsOnly && !findingCount && questionCount === 0
      ? "Só sugestões"
      : questionCount > 0
        ? `${achados(findingCount ?? 0)} · ${perguntas(questionCount)}`
        : noFindings || result === "clean"
          ? "Nada a sinalizar"
          : findingCount !== undefined && findingCount > 0
            ? achados(findingCount)
            : result === "findings_local" || result === "findings_posted"
              ? "Achados gerados"
              : "Ver achados e detalhes";
  if (publication === "failed")
    return { status: "Publicação falhou", detail: "Abra o resultado para tentar de novo", state: "failed" };
  if (publication === "pending" || publication === "posting")
    return { status: publication === "posting" ? "Publicando…" : "Aguardando publicação", detail, state: "queued" };
  if (publication === "superseded")
    return {
      status: "Resultado desatualizado",
      detail: "Reexecute para as alterações mais recentes",
      state: "superseded",
    };
  if (card.lane === "ready" || result === "awaiting_action")
    return { status: "Precisa da sua decisão", detail, state: "awaiting_action" };
  if (result === "clean") return { status: "Concluído", detail: "Nada a sinalizar", state: "completed" };
  if (result === "skipped" || run?.status === "skipped")
    return { status: "Ignorado", detail: "Ver detalhes da execução", state: "completed" };
  if (publication === "posted" || run?.status === "posted" || result === "findings_posted")
    return { status: "Publicado", detail, state: "completed" };
  if (run?.status === "approved") return { status: "Marcado como revisado", detail, state: "completed" };
  if (run || result) return { status: "Gerado", detail, state: "completed" };
  return { status: "Aguardando revisão", detail: "Execute uma revisão para gerar resultados", state: "pending" };
}

interface ReviewWorkspaceState {
  view: string | null;
  processesOpen: boolean;
  restoreProcesses: boolean;
}

export function isReviewResultView(view: string | null) {
  return view === "overview" || view === "ready";
}

type ReviewWorkspaceAction =
  | { type: "inspect"; view: string }
  | { type: "close" | "toggle-processes" }
  | { type: "reset" | "resize"; wide: boolean };

export function reviewWorkspaceReducer(
  state: ReviewWorkspaceState,
  action: ReviewWorkspaceAction,
): ReviewWorkspaceState {
  switch (action.type) {
    case "inspect":
      return {
        view: action.view,
        processesOpen: false,
        restoreProcesses: state.view === null ? state.processesOpen : state.restoreProcesses,
      };
    case "close":
      return { ...state, view: null, processesOpen: state.restoreProcesses };
    case "toggle-processes":
      return { ...state, processesOpen: !state.processesOpen, restoreProcesses: !state.processesOpen };
    case "reset":
      return { view: null, processesOpen: action.wide, restoreProcesses: action.wide };
    case "resize":
      return { ...state, processesOpen: state.view === null && action.wide, restoreProcesses: action.wide };
  }
}

export type ReviewFlowState =
  | "pending"
  | "queued"
  | "running"
  | "completed"
  | "awaiting_action"
  | "failed"
  | "cancelled"
  | "superseded"
  | "unknown"
  | "skipped"
  | "partial"
  | "unavailable";

export interface ReviewFlowNodeData extends Record<string, unknown> {
  phase: ReviewPhase;
  state: ReviewFlowState;
  detail: string;
  metrics?: ReviewFlowMetrics;
  reverse: boolean;
  lastCompleted?: boolean;
  chunk?: ReviewChunkView;
  chunkGroup?: ReviewChunks;
  onSelect?: () => void;
  control?: ReviewFlowControl;
  result?: ReviewResultSummary;
}

export type ReviewFlowNode = Node<ReviewFlowNodeData, "reviewPhase" | "reviewControl">;

const FALLBACK_PHASES: ReviewPhase[] = [
  { id: "index", label: "Preparar repositório", status: "pending" },
  { id: "context", label: "Contexto pronto", status: "pending" },
  { id: "review", label: "Revisado", status: "pending" },
  { id: "validate", label: "Validado", status: "pending" },
  { id: "ready", label: "Pronto", status: "pending" },
];

const DONE_STATES = new Set(["ok", "done", "ready", "completed", "cached", "skipped"]);
const FAILURE_STATES = new Set(["failed", "error", "cancelled"]);
const TERMINAL_FAILURES = new Set(["failed", "cancelled", "superseded"]);

export function parseReviewPhases(raw: string | null | undefined): ReviewPhase[] {
  if (!raw) return FALLBACK_PHASES;
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return FALLBACK_PHASES;
    const phases = parsed.filter(isReviewPhase);
    return phases.length > 0 ? phases : FALLBACK_PHASES;
  } catch {
    return FALLBACK_PHASES;
  }
}

function isReviewPhase(value: unknown): value is ReviewPhase {
  if (!value || typeof value !== "object") return false;
  const phase = value as Record<string, unknown>;
  return typeof phase.id === "string" && typeof phase.label === "string" && typeof phase.status === "string";
}

export function phaseLabel(phase: Pick<ReviewPhase, "id" | "label">) {
  if (phase.id === "context") return "Contexto";
  if (phase.id === "chunks") return "Chunks";
  if (phase.id === "merge") return "Mesclar";
  if (phase.id === "final") return "Final";
  if (phase.id === "validate") return "Validar";
  if (phase.id === "review") return "Revisão";
  if (phase.id === "ready") return "Resultado da revisão";
  return phase.label;
}

function phaseState(
  phase: ReviewPhase,
  index: number,
  phases: ReviewPhase[],
  runStatus: string | null,
): ReviewFlowState {
  const raw = phase.status.toLowerCase();
  const activeIndex = phases.findIndex(
    (item) => FAILURE_STATES.has(item.status.toLowerCase()) || item.status.toLowerCase() === "running",
  );
  const pendingIndex = phases.findIndex((item) => !DONE_STATES.has(item.status.toLowerCase()));
  const failureIndex = TERMINAL_FAILURES.has(runStatus ?? "")
    ? activeIndex >= 0
      ? activeIndex
      : pendingIndex >= 0
        ? pendingIndex
        : phases.length - 1
    : -1;
  if (raw === "cancelled") return "cancelled";
  if (raw === "failed" || raw === "error") return runStatus === "superseded" ? "superseded" : "failed";
  if (TERMINAL_FAILURES.has(runStatus ?? "") && index === failureIndex) {
    return runStatus === "cancelled" ? "cancelled" : runStatus === "superseded" ? "superseded" : "failed";
  }
  if (DONE_STATES.has(raw)) return "completed";
  if (raw === "running") return TERMINAL_FAILURES.has(runStatus ?? "") ? "failed" : "running";
  if (raw === "queued" || (runStatus === "queued" && index === 0)) return "queued";
  if (["reviewing", "running"].includes(runStatus ?? "") && activeIndex < 0 && index === pendingIndex) return "running";
  if (runStatus === "failed" && index === 0) return "failed";
  return "pending";
}

function phaseDetail(phase: ReviewPhase, state: ReviewFlowState) {
  if (typeof phase.detail === "string" && phase.detail) return phase.detail;
  if (phase.id === "index" && state === "pending")
    return "Preparando o repositório local; o grafo do head fixo é montado em Contexto";
  if (state === "awaiting_action") return "Aguardando sua decisão";
  if (state === "completed") return "Concluído";
  if (state === "running") return "Processando agora";
  if (state === "queued") return "Aguardando um worker";
  if (state === "failed") return "Revisão falhou";
  if (state === "cancelled") return "Interrompido pelo usuário";
  if (state === "superseded") return "Substituído por mudança no head ou na base do MR";
  if (phase.id === "context") return "Selecionando arquivos alterados e relacionados";
  if (phase.id === "chunks") return "Executando chunks de revisão em paralelo";
  if (phase.id === "merge") return "Combinando achados";
  if (phase.id === "final") return "Integrando a revisão final";
  if (phase.id === "validate") return "Verificando a saída da revisão";
  if (phase.id === "review") return "Executando a revisão por IA";
  if (phase.id === "ready") return "Aguardando sua decisão";
  return phase.label;
}

export interface ChunkProgress {
  done: number;
  total: number;
  running: number;
  chunks: Array<{ id: number; status: string; duration?: string }>;
  currentId: number | null;
  currentLog: string | null;
}

export interface ReviewChunkView {
  cached: boolean;
  validating: boolean;
  queuedDurationMs: number | null;
  error: string | null;
  attempts: number;
  id: number;
  state: ReviewFlowState;
  metrics: ReviewFlowMetrics;
  reportedTime: boolean;
  startedAt?: number;
  finishedAt?: number;
  detail: string;
  log: string | null;
}

export interface ReviewChunks {
  runId: number | null;
  items: ReviewChunkView[];
  total: number | null;
  counts: Record<ReviewFlowState, number>;
}

const MAX_VISIBLE_CHUNKS = 1000;
const validChunkId = (id: number): boolean => Number.isSafeInteger(id) && id > 0 && id <= MAX_VISIBLE_CHUNKS;

const CHUNK_STATUS_ALIASES: Record<string, string> = {
  done: "done",
  cached: "cached",
  validating: "validating",
  running: "running",
  queued: "queued",
  failed: "failed",
  error: "error",
  cancelled: "cancelled",
  superseded: "superseded",
  concluído: "done",
  concluido: "done",
  "em cache": "cached",
  validando: "validating",
  "em execução": "running",
  "em execucao": "running",
  "na fila": "queued",
  falhou: "failed",
  erro: "error",
  cancelado: "cancelled",
  substituído: "superseded",
  substituido: "superseded",
};

export function parseChunkProgress(raw: string): ChunkProgress | null {
  const tail = raw.slice(0, 32_768).trimStart();
  // Accept English (historical runs) and Portuguese (current backend) progress lines.
  const summary = tail.match(
    /^(?:chunks progress:\s+(\d+)\/(\d+)\s+done,\s+(\d+)\s+running|progresso dos chunks:\s+(\d+)\/(\d+)\s+conclu[ií]dos,\s+(\d+)\s+em execu[cç][aã]o)\b/i,
  );
  if (!summary) return null;
  const [done, total, running] = (summary[1] ? summary.slice(1, 4) : summary.slice(4, 7)).map(Number);
  if (!validChunkId(total) || done + running > total) return null;
  const header = /^(?:chunk status|status dos chunks):\s*\r?$/im.exec(tail);
  const chunks: ChunkProgress["chunks"] = [];
  if (header) {
    for (const line of tail
      .slice(header.index + header[0].length)
      .trimStart()
      .split(/\r?\n/)) {
      const match = line.match(
        /^\s*(\d+)\s+(done|cached|validating|running|queued|failed|error|cancelled|superseded|conclu[ií]do|em cache|validando|em execu[cç][aã]o|na fila|falhou|erro|cancelado|substitu[ií]do)(?:\s+((?:\d+h)?(?:\d+m)?\d+s))?\s*$/i,
      );
      if (!match) break;
      const id = Number(match[1]);
      const status = CHUNK_STATUS_ALIASES[match[2].toLowerCase()];
      if (status && id <= total && validChunkId(id) && !chunks.some((chunk) => chunk.id === id))
        chunks.push({ id, status, duration: match[3] });
    }
  }
  const current = header ? /^(?:current chunk|chunk atual):\s+(\d+)\s*\r?$/im.exec(tail) : null;
  const currentId =
    current && validChunkId(Number(current[1])) && Number(current[1]) <= total ? Number(current[1]) : null;
  const body = current ? tail.slice(current.index + current[0].length).trim() : "";
  const owner = body.match(/^chunk\s+(\d+)\/\d+:/i);
  const currentLog = owner && Number(owner[1]) === currentId ? body : null;
  return { done, total, running, chunks, currentId, currentLog };
}

function chunkDuration(raw: string | undefined): number | null {
  const parts = raw?.match(/^(?:(\d+)h)?(?:(\d+)m)?(\d+)s$/);
  const ms = parts ? (Number(parts[1] ?? 0) * 3600 + Number(parts[2] ?? 0) * 60 + Number(parts[3])) * 1000 : null;
  return ms !== null && Number.isFinite(ms) ? ms : null;
}

function chunkState(raw: string | undefined): ReviewFlowState {
  if (raw && DONE_STATES.has(raw)) return "completed";
  if (raw === "validating") return "running";
  if (raw === "error" || raw === "failed") return "failed";
  if (raw === "running" || raw === "queued" || raw === "cancelled" || raw === "superseded") return raw;
  return "unknown";
}

function isStructuredChunk(value: unknown): value is ReviewChunkProgress {
  if (!value || typeof value !== "object") return false;
  const item = value as Record<string, unknown>;
  return (
    typeof item.id === "number" &&
    validChunkId(item.id) &&
    typeof item.status === "string" &&
    ["queued", "running", "validating", "done", "cached", "failed", "cancelled"].includes(item.status) &&
    Number.isFinite(item.queuedAt) &&
    [item.startedAt, item.executionFinishedAt, item.finishedAt].every(
      (at) => at === null || (typeof at === "number" && Number.isFinite(at)),
    )
  );
}

type TokenUsage = Pick<ReviewRunStage, "inputTokens" | "outputTokens" | "cacheReadTokens" | "cacheCreationTokens">;

/** Tokens a run processed. Cache reads are left out: every model turn re-reads the cached
 *  context, so they dwarf the real work and cost a fraction; `tokenSplit` shows them apart. */
export function totalTokens(usage: TokenUsage) {
  return [usage.inputTokens, usage.outputTokens, usage.cacheCreationTokens].reduce(
    (sum, value) => sum + (Number.isFinite(value) && value > 0 ? value : 0),
    0,
  );
}

export function stageTokens(stage: ReviewRunStage): number | null {
  const total = totalTokens(stage);
  if (total > 0) return total;
  return stage.cacheHit || stage.status === "cached" ? 0 : null;
}

export function buildReviewChunks(
  phases: ReviewPhase[],
  run: ReviewRun | null,
  stages: ReviewRunStage[],
  previous?: ReviewChunks | null,
  now = Date.now(),
): ReviewChunks {
  const prior = run && previous?.runId === run.id ? previous : null;
  const counts: ReviewChunks["counts"] = {
    pending: 0,
    queued: 0,
    running: 0,
    completed: 0,
    awaiting_action: 0,
    failed: 0,
    cancelled: 0,
    superseded: 0,
    unknown: 0,
    skipped: 0,
    partial: 0,
    unavailable: 0,
  };
  const parent = phases.find((phase) => phase.id === "chunks");
  if (!parent) return { runId: run?.id ?? null, items: [], total: null, counts };
  const structured = Array.isArray(parent.chunks)
    ? parent.chunks.filter(isStructuredChunk).slice(0, MAX_VISIBLE_CHUNKS)
    : null;
  const snapshot = structured
    ? null
    : (parseChunkProgress(run?.tail ?? "") ??
      parseChunkProgress(typeof parent.log === "string" ? parent.log : "") ??
      parseChunkProgress(typeof parent.detail === "string" ? parent.detail : ""));
  const completed =
    typeof parent.detail === "string"
      ? (parent.detail.match(/^Completed (\d+) chunk reviews\b/) ??
        parent.detail.match(/^Concluídas? (\d+) revisões? de chunk\b/))
      : null;
  const declaredTotal =
    structured?.length ??
    snapshot?.total ??
    (completed && validChunkId(Number(completed[1])) ? Number(completed[1]) : (prior?.total ?? null));
  const rows = [
    ...new Map(
      stages
        .filter(
          (stage) =>
            stage.stage === "chunk" &&
            stage.chunkId !== null &&
            validChunkId(stage.chunkId) &&
            (!run || stage.runId === run.id),
        )
        .map((stage) => [stage.id, stage]),
    ).values(),
  ];
  const ids = new Set<number>(
    !structured && declaredTotal ? Array.from({ length: declaredTotal }, (_, index) => index + 1) : [],
  );
  for (const chunk of structured ?? []) ids.add(chunk.id);
  for (const row of rows) if (row.chunkId !== null) ids.add(row.chunkId);
  for (const chunk of snapshot?.chunks ?? []) ids.add(chunk.id);
  for (const chunk of prior?.items ?? []) ids.add(chunk.id);
  const items = [...ids]
    .sort((a, b) => a - b)
    .map((id): ReviewChunkView => {
      const records = rows.filter((row) => row.chunkId === id).sort((a, b) => a.id - b.id);
      const latest = records.at(-1);
      const live = structured?.find((chunk) => chunk.id === id);
      const reported = snapshot?.chunks.find((chunk) => chunk.id === id);
      const old = prior?.items.find((chunk) => chunk.id === id);
      const remembered =
        old && ["completed", "failed", "cancelled", "superseded"].includes(old.state) ? old.state : undefined;
      let state = chunkState(latest?.status ?? live?.status ?? remembered ?? reported?.status ?? old?.state);
      if (!structured && DONE_STATES.has(parent.status) && ["running", "queued", "unknown"].includes(state))
        state = "completed";
      if (run && !["running", "pending"].includes(run.status)) {
        if (state === "running")
          state = run.status === "cancelled" ? "cancelled" : run.status === "superseded" ? "superseded" : "unknown";
        if (state === "queued" && TERMINAL_FAILURES.has(run.status))
          state = run.status === "superseded" ? "superseded" : "cancelled";
      }
      const cached = Boolean(
        latest?.cacheHit || latest?.status === "cached" || live?.cached || live?.status === "cached" || old?.cached,
      );
      const validating = state === "running" && live?.status === "validating";
      const starts = records.map((row) => row.createdAt).filter(Number.isFinite);
      const ends = records
        .map((row) => row.finishedAt)
        .filter((value): value is number => value !== null && Number.isFinite(value));
      const startedAt = cached
        ? undefined
        : live
          ? (live.startedAt ?? undefined)
          : starts.length
            ? Math.min(...starts)
            : old?.startedAt;
      const finishedAt = live?.finishedAt ?? (ends.length ? Math.max(...ends) : old?.finishedAt);
      const executionEnd = live?.executionFinishedAt ?? finishedAt ?? (live && state === "running" ? now : undefined);
      const measured =
        startedAt !== undefined && executionEnd !== undefined ? Math.max(0, executionEnd - startedAt) : null;
      const queueEnd =
        startedAt ??
        finishedAt ??
        (run && ["running", "pending"].includes(run.status) ? now : (run?.finishedAt ?? undefined));
      const queuedDurationMs =
        live && queueEnd !== undefined ? Math.max(0, queueEnd - live.queuedAt) : (old?.queuedDurationMs ?? null);
      const durationMs = cached
        ? 0
        : (measured ?? chunkDuration(reported?.duration) ?? old?.metrics.durationMs ?? null);
      const tokens = records.reduce((sum, row) => sum + (stageTokens(row) ?? 0), 0);
      const detail = cached
        ? "Cache de snapshot validado reutilizado; sem nova inferência."
        : validating
          ? "Verificando evidências citadas, cobertura e âncoras localmente."
          : state === "unknown"
            ? "Nenhum status final registrado para este chunk. Não se assume falha nem conclusão."
            : `${flowStateLabel(state)} · ${latest?.fileCount ?? live?.fileCount ?? "Desconhecido"} arquivos atribuídos a este chunk.`;
      counts[state] += 1;
      return {
        id,
        state,
        cached,
        validating,
        queuedDurationMs,
        error: live?.error ?? old?.error ?? null,
        attempts: live?.attempts ?? old?.attempts ?? 0,
        startedAt,
        finishedAt,
        detail,
        reportedTime: !live && measured === null && durationMs !== null,
        metrics: {
          durationMs,
          tokens: tokens > 0 ? tokens : cached ? 0 : records.length ? null : (old?.metrics.tokens ?? null),
          partial:
            state !== "completed" ||
            (records.length
              ? records.some((row) => stageTokens(row) === null)
              : cached
                ? false
                : (old?.metrics.partial ?? true)),
        },
        log:
          typeof live?.log === "string"
            ? live.log.slice(-8192)
            : ((snapshot?.currentId === id ? snapshot.currentLog : null)?.slice(-8192) ?? old?.log ?? null),
      };
    });
  return {
    runId: run?.id ?? null,
    items,
    counts,
    total: declaredTotal !== null && (items.at(-1)?.id ?? 0) <= declaredTotal ? declaredTotal : null,
  };
}

export function chunkCountsLabel(chunks: ReviewChunks, inferenceSkipped = false) {
  if (!chunks.items.length)
    return inferenceSkipped ? "Nenhuma execução de modelo iniciada" : "Detalhes do chunk indisponíveis";
  const { completed, running, queued, failed, cancelled, superseded, unknown } = chunks.counts;
  const validating = chunks.items.filter((chunk) => chunk.validating).length;
  return [
    `${chunks.total === null ? `${chunks.items.length} registrados · ` : ""}${unknown ? "Conhecidos: " : ""}${completed}${chunks.total !== null ? `/${chunks.total}` : ""} concluídos`,
    `${Math.max(0, running - validating)} running`,
    `${queued} queued`,
    ...(validating ? [`${validating} validating`] : []),
    ...(chunks.items.some((chunk) => chunk.cached)
      ? [`${chunks.items.filter((chunk) => chunk.cached).length} cached`]
      : []),
    ...(failed ? [`${failed} failed`] : []),
    ...(cancelled + superseded ? [`${cancelled + superseded} stopped`] : []),
    ...(unknown ? [`${unknown} status unavailable`] : []),
  ].join(" · ");
}

export function buildReviewFlow(
  phases: ReviewPhase[],
  runStatus: string | null,
  columns = 3,
  chunks?: ReviewChunks,
  control?: ReviewFlowControl,
) {
  const width = Math.max(1, Math.min(4, Math.floor(columns)));
  let row = 0;
  let column = 0;
  let y = 64;
  let rowHeight = 92;
  const children: ReviewFlowNode[] = [];
  const states = phases.map((phase, index) => phaseState(phase, index, phases, runStatus));
  const lastCompleted = states.reduce((last, state, index) => (state === "completed" ? index : last), -1);
  const nodes: ReviewFlowNode[] = phases.map((phase, index) => {
    const group = phase.id === "chunks" && chunks?.items.length ? chunks : null;
    const span = group ? Math.min(2, width) : 1;
    if (column + span > width) {
      row += 1;
      column = 0;
      y += rowHeight + 76;
      rowHeight = 92;
    }
    const reverse = row % 2 === 1;
    const state = states[index];
    const groupWidth = span * 246 - 30;
    const childTop = span === 1 ? 176 : 132;
    const height = group ? childTop + Math.ceil(group.items.length / span) * 104 : 92;
    const node: ReviewFlowNode = {
      id: phase.id,
      type: "reviewPhase",
      initialWidth: group ? groupWidth : 216,
      initialHeight: height,
      position: { x: (reverse ? width - column - span : column) * 246, y },
      ...(group ? { style: { width: groupWidth, height }, zIndex: 0 } : {}),
      data: {
        phase,
        state,
        reverse,
        lastCompleted: index === lastCompleted,
        detail: phaseDetail(phase, state),
        ...(phase.id === "chunks" && chunks ? { chunkGroup: chunks } : {}),
      },
    };
    if (group) {
      const childWidth = (groupWidth - 24 - (span - 1) * 12) / span;
      group.items.forEach((chunk, index) => {
        const id = `chunk:${chunk.id}`;
        const childPhase: ReviewPhase = {
          id,
          label: `Chunk ${chunk.id}`,
          status: chunk.state,
          detail: chunk.detail,
          startedAt: chunk.startedAt,
          finishedAt: chunk.finishedAt,
          ...(chunk.log ? { log: chunk.log } : {}),
        };
        children.push({
          id,
          type: "reviewPhase",
          parentId: "chunks",
          extent: "parent",
          zIndex: 1,
          position: { x: 12 + (index % span) * (childWidth + 12), y: childTop + Math.floor(index / span) * 104 },
          initialWidth: childWidth,
          initialHeight: 92,
          style: { width: childWidth, height: 92 },
          data: {
            phase: childPhase,
            state: chunk.state,
            detail: chunk.detail,
            reverse: false,
            chunk,
            metrics: chunk.metrics,
          },
        });
      });
    }
    column += span;
    rowHeight = Math.max(rowHeight, height);
    return node;
  });
  const edges: Edge[] = phases.slice(0, -1).map((phase, index) => {
    const next = phases[index + 1];
    const sourceState = nodes[index].data.state;
    const targetState = nodes[index + 1].data.state;
    return {
      id: `${phase.id}-${next.id}`,
      source: phase.id,
      target: next.id,
      type: "smoothstep",
      animated: targetState === "running",
      className: `review-flow-edge-${["running", "awaiting_action", "failed", "cancelled", "superseded"].includes(targetState) ? targetState : sourceState === "completed" && targetState === "completed" ? "completed" : "pending"}`,
    };
  });
  const first = nodes[0];
  if (control && first) {
    nodes.unshift({
      id: "review-control",
      type: "reviewControl",
      position: { x: first.position.x - 96, y: first.position.y + 22 },
      width: 48,
      height: 48,
      selectable: false,
      style: { pointerEvents: "all" },
      data: {
        phase: { id: "review-control", label: control.label, status: "pending" },
        state: "pending",
        detail: control.label,
        reverse: false,
        control,
      },
    });
    edges.unshift({
      id: `review-control-${first.id}`,
      source: "review-control",
      target: first.id,
      type: "smoothstep",
      className: `review-flow-edge-${first.data.state}`,
    });
  }
  return { nodes: nodes.flatMap((node) => (node.id === "chunks" ? [node, ...children] : [node])), edges };
}

export interface ReviewFlowMetrics {
  durationMs: number | null;
  tokens: number | null;
  partial: boolean;
}

const LOCAL_PHASES = new Set(["index", "context", "merge", "validate", "ready"]);
const AI_STAGES: Record<string, string> = { review: "single", chunks: "chunk", final: "final" };

export function phaseStages(id: string, stages: ReviewRunStage[]) {
  const chunk = id.match(/^chunk:(\d+)$/);
  return stages.filter((stage) =>
    chunk ? stage.stage === "chunk" && stage.chunkId === Number(chunk[1]) : stage.stage === AI_STAGES[id],
  );
}

function measuredTime(value: number | null | undefined) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function phaseMetrics(
  phase: ReviewPhase,
  state: ReviewFlowState,
  stages: ReviewRunStage[],
  run: ReviewRun | null,
  now: number,
): ReviewFlowMetrics {
  const rows = phaseStages(phase.id, stages);
  const starts = rows.map((row) => row.createdAt).filter(Number.isFinite);
  const finishes = rows
    .map((row) => row.finishedAt)
    .filter((value): value is number => value != null && Number.isFinite(value));
  const start = measuredTime(phase.startedAt) ?? (starts.length ? Math.min(...starts) : null);
  const end =
    measuredTime(phase.finishedAt) ??
    (state === "running"
      ? now
      : ["failed", "cancelled", "superseded"].includes(state)
        ? measuredTime(run?.finishedAt)
        : finishes.length
          ? Math.max(...finishes)
          : null);
  const durationMs = start !== null && end !== null ? Math.max(0, end - start) : null;
  if (state === "pending" || state === "queued") return { durationMs, tokens: null, partial: false };
  if (LOCAL_PHASES.has(phase.id) || phase.inferenceSkipped) return { durationMs, tokens: 0, partial: false };
  const recorded = rows.reduce((sum, row) => sum + totalTokens(row), 0);
  const runTokens = run ? totalTokens(run) : 0;
  const singleFallback = phase.id === "review" && runTokens > 0 ? runTokens : null;
  const knownZero = rows.length > 0 && rows.every((row) => stageTokens(row) !== null);
  return {
    durationMs,
    tokens: recorded > 0 ? recorded : knownZero ? 0 : singleFallback,
    partial: state !== "completed" || rows.some((row) => stageTokens(row) === null),
  };
}

export function runMetrics(run: ReviewRun | null, stages: ReviewRunStage[], now: number): ReviewFlowMetrics {
  if (!run) return { durationMs: null, tokens: null, partial: false };
  const live = ["running", "pending"].includes(run.status);
  const start = measuredTime(run.startedAt);
  const end = measuredTime(run.finishedAt) ?? (live ? now : null);
  const durationMs = start !== null && end !== null ? Math.max(0, end - start) : null;
  const recorded = stages.reduce((sum, row) => sum + totalTokens(row), 0);
  const tokens = Math.max(totalTokens(run), recorded);
  const localOnly = Boolean(
    run.phaseJson &&
      live &&
      !parseReviewPhases(run.phaseJson).some((phase) => AI_STAGES[phase.id] && phase.status !== "pending"),
  );
  const knownZero = stages.length > 0 && stages.every((stage) => stageTokens(stage) !== null);
  const aiPhases = parseReviewPhases(run.phaseJson).filter((phase) => AI_STAGES[phase.id]);
  const skipped =
    aiPhases.length > 0 && aiPhases.every((phase) => phase.inferenceSkipped) && stages.length === 0 && tokens === 0;
  return {
    durationMs,
    tokens: tokens > 0 ? tokens : localOnly || knownZero || skipped ? 0 : null,
    partial:
      !skipped &&
      (live ||
        ["failed", "cancelled", "superseded"].includes(run.status) ||
        stages.some((stage) => stageTokens(stage) === null)),
  };
}

export function formatFlowDuration(ms: number | null) {
  if (ms === null) return "—";
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}

export function formatFlowTokens(tokens: number | null) {
  if (tokens === null) return "Tokens indisponíveis";
  const value =
    tokens >= 1_000_000
      ? `${(tokens / 1_000_000).toLocaleString(undefined, { maximumFractionDigits: 1 })}M`
      : tokens >= 1000
        ? `${(tokens / 1000).toLocaleString(undefined, { maximumFractionDigits: 1 })}k`
        : tokens.toLocaleString();
  return `${value} tokens`;
}

export function flowStateLabel(state: ReviewFlowState) {
  if (state === "awaiting_action") return "Precisa da sua decisão";
  if (state === "skipped") return "Ignorado";
  if (state === "partial") return "Contexto parcial";
  if (state === "unavailable") return "Contexto indisponível";
  if (state === "completed") return "Concluído";
  if (state === "running") return "Em execução";
  if (state === "queued") return "Na fila";
  if (state === "failed") return "Falhou";
  if (state === "cancelled") return "Cancelado";
  if (state === "superseded") return "Substituído";
  if (state === "unknown") return "Desconhecido";
  return "Pendente";
}
