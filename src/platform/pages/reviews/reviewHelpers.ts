import type { Tone } from "../../../components/DesignSystem";
import type { ReviewCard, ReviewRisk, ReviewRun } from "../../../types";
import { formatError } from "../../lib/utils";

export type ReviewDecision = "approve_post" | "request_changes" | "comment" | "mark_reviewed" | "skip";

export function itemTime(value: number, now: number) {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "";
  const sameDay = date.toDateString() === new Date(now).toDateString();
  return sameDay
    ? date.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleDateString("pt-BR", { month: "short", day: "numeric" });
}

/** Most recently updated first; cards without a usable time sink to the bottom. */
export function byRecent(items: ReviewCard[]) {
  const timestamp = (card: ReviewCard) => {
    const value = Number(card.updatedAt);
    return Number.isFinite(value) ? value : Number.NEGATIVE_INFINITY;
  };
  return [...items].sort((a, b) => timestamp(b) - timestamp(a) || b.id - a.id);
}

export function streamStatusLabel(state: string) {
  if (state === "live") return "Ao vivo via SSE";
  if (state === "connecting" || state === "reconnecting") return "Conectando monitor ao vivo";
  if (state === "fallback") return "Polling de fallback";
  if (state === "closed") return "Processo concluído";
  return "Monitor ocioso";
}

interface InlineComment {
  path: string;
  line: number | string;
  body: string;
  status?: string;
  duplicateUrl?: string;
  duplicateAuthor?: string;
}

function cleanCommentBody(body: string) {
  return body
    .replace(/^<!-- ocr:fingerprint=.*?-->\n?/m, "")
    .replace(/\n{4,}/g, "\n\n\n")
    .trim();
}

export function parseComments(json: string | null): InlineComment[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    if (!Array.isArray(parsed?.comments)) return [];
    return parsed.comments.map((comment: Record<string, unknown>) => ({
      path: String(comment.path ?? ""),
      line: (comment.line as number | string) ?? "",
      body: cleanCommentBody(String(comment.body ?? "")),
      status: typeof comment.status === "string" ? comment.status : undefined,
      duplicateUrl: typeof comment.duplicateUrl === "string" ? comment.duplicateUrl : undefined,
      duplicateAuthor: typeof comment.duplicateAuthor === "string" ? comment.duplicateAuthor : undefined,
    }));
  } catch {
    return [];
  }
}

export const fmtTokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 100000 ? 0 : 1)}k` : `${n}`);
export const shortSha = (sha: string | null | undefined) => (sha ? sha.slice(0, 8) : "?");

export function durationLabel(run: ReviewRun) {
  if (!run.startedAt) return null;
  const seconds = Math.round(((run.finishedAt ?? Date.now()) - run.startedAt) / 1000);
  if (seconds < 0) return null;
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

export function parseRisk(run: ReviewRun): ReviewRisk | null {
  if (run.riskJson) {
    try {
      return JSON.parse(run.riskJson) as ReviewRisk;
    } catch {
      return null;
    }
  }
  if (run.riskScore === null || !run.riskLevel) return null;
  return {
    score: run.riskScore,
    level: run.riskLevel,
    summary: `risco de merge ${run.riskLevel}`,
    autoMergeEligible: false,
    drivers: [],
  };
}

export function reviewError(cause: unknown) {
  const message = formatError(cause);
  return message.includes("route not found")
    ? "API de revisão indisponível no servidor em execução. Reconstrua/reinicie o backend Docker para carregar as novas rotas de revisão."
    : message;
}

const LANE_LABELS: Record<string, string> = {
  manual_required: "Precisa de você",
  needs_setup: "Precisa de configuração",
  ready: "Pronto para publicar",
  reviewing: "Revisando",
  queued: "Na fila",
  open: "Aberto",
  draft: "Rascunho",
  auto_skipped: "Ignorado",
  skipped: "Ignorado",
  size_skipped: "Grande demais",
  auto_posted: "Achados publicados",
  posted: "Achados publicados",
  reviewed_cleanly: "Nada a sinalizar",
  approved: "Aprovado",
  merged: "Mesclado",
  closed: "Fechado",
  failed: "Falhou",
};

/** A short status for a review lane. */
function labelLane(lane: string) {
  return LANE_LABELS[lane] ?? lane.replaceAll("_", " ");
}

/** A card's status: its lane, except that a review published with only suggestions or questions says so. */
export function labelCard(card: Pick<ReviewCard, "lane" | "suggestionsOnly" | "questionsOnly">) {
  if (!["auto_posted", "posted"].includes(card.lane)) return labelLane(card.lane);
  if (card.suggestionsOnly) return "Sugestões publicadas";
  return card.questionsOnly ? "Perguntas publicadas" : labelLane(card.lane);
}

/** Each outcome gets its own colour: posted findings are green, a clean review stays grey. */
export function laneTone(lane: string): Tone {
  if (["manual_required", "needs_setup", "ready"].includes(lane)) return "warning";
  if (["reviewing", "queued"].includes(lane)) return "brand";
  if (["auto_posted", "posted", "approved"].includes(lane)) return "success";
  if (lane === "merged") return "teal";
  if (lane === "failed") return "danger";
  return "neutral";
}
