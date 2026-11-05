import type { FindingAuditView, FindingCandidate } from "../../types";
import { required } from "./utils";

export interface FindingSelectionDraft {
  prId: number;
  runId: number;
  revision: number;
  selectedIds: string[];
}

export function selectedFindingIds(view: FindingAuditView) {
  return (
    view.audit?.candidates
      .filter((candidate) => candidate.selected && !candidate.blockedReason)
      .map((candidate) => candidate.id) ?? []
  );
}

export function findingSelectionDirty(view: FindingAuditView, draft: FindingSelectionDraft | null) {
  if (!draft || draft.runId !== view.runId || draft.prId !== view.prId) return false;
  const saved = selectedFindingIds(view);
  return (
    draft.revision !== view.revision ||
    saved.length !== draft.selectedIds.length ||
    saved.some((id) => !draft.selectedIds.includes(id))
  );
}

export function canDecideFindings(view: FindingAuditView | undefined, draft: FindingSelectionDraft | null) {
  if (!view) return false;
  if (!view.requiresConfirmation) return true;
  if (!view.audit?.complete) return false;
  if (view.audit.candidates.length === 0) return view.editable;
  return Boolean(view.editable && view.selectionSaved && !findingSelectionDirty(view, draft));
}

export async function prepareEmptyFindingDecision(
  prId: number,
  runId: number,
  headSha: string,
  load: () => Promise<FindingAuditView>,
  save: (revision: number) => Promise<FindingAuditView>,
) {
  const validate = (view: FindingAuditView) => {
    if (
      view.prId !== prId ||
      view.runId !== runId ||
      !view.requiresConfirmation ||
      !view.editable ||
      !view.audit?.complete ||
      view.audit.headSha !== headSha ||
      view.audit.candidates.length !== 0
    ) {
      throw new Error("A revisão mudou ou a auditoria deixou de estar completa e vazia. Recarregue antes de decidir.");
    }
  };
  const current = await load();
  validate(current);
  if (current.selectionSaved) return current;
  const saved = await save(current.revision);
  validate(saved);
  if (!saved.selectionSaved) {
    throw new Error("A seleção vazia de achados não foi salva. Recarregue antes de decidir.");
  }
  return saved;
}

const POINT_MARKER = /\[(Achado|Pergunta|Finding|Question)\]/i;
const POINT_MARKER_PREFIX = /^\[(Achado|Pergunta|Finding|Question)\]/;

/** Whether a marker name is a finding (Achado/Finding) rather than a question. */
function isFindingMarker(name: string) {
  return /^(Achado|Finding)$/i.test(name);
}

/** The `[Achado]`/`[Pergunta]` (or legacy English) titles in a body, outside code fences. */
function pointTitles(body: string) {
  let fence = false;
  const titles: string[] = [];
  for (const line of body.split("\n")) {
    if (/^\s*(```|~~~)/.test(line)) {
      fence = !fence;
      continue;
    }
    const match = fence ? null : line.replace(/^[\s*_>#-]+/, "").match(POINT_MARKER_PREFIX);
    if (match) titles.push(match[1]);
  }
  return titles;
}

export function reviewPointKind(body: string): "finding" | "question" | "mixed" | "unknown" {
  const titles = pointTitles(body);
  const finding = titles.some(isFindingMarker);
  const question = titles.some((title) => !isFindingMarker(title));
  return finding && question ? "mixed" : finding ? "finding" : question ? "question" : "unknown";
}

/** Findings and questions a stored review holds, like the backend's `point_counts`: an inline comment
 * that is both counts as both and one without a marker as a finding; the body adds each point it lists. */
export function storedPointCounts(commentBodies: string[], reviewBody: string | null) {
  const counts = { findings: 0, questions: 0 };
  for (const body of commentBodies) {
    const kind = reviewPointKind(body);
    if (kind !== "question") counts.findings += 1;
    if (kind === "question" || kind === "mixed") counts.questions += 1;
  }
  for (const title of pointTitles(reviewBody ?? "")) {
    counts[isFindingMarker(title) ? "findings" : "questions"] += 1;
  }
  return counts;
}

export const candidateKind = (candidate: FindingCandidate) => candidate.kind ?? reviewPointKind(candidate.body);

export function groupFindingCandidates(candidates: FindingCandidate[]) {
  const groups = [
    { id: "eligible", title: "Achados elegíveis", open: true, candidates: [] as FindingCandidate[] },
    { id: "questions", title: "Perguntas — publicação opcional", open: true, candidates: [] as FindingCandidate[] },
    { id: "discarded", title: "Descartados pela IA", open: false, candidates: [] as FindingCandidate[] },
    { id: "blocked", title: "Bloqueados pela validação", open: false, candidates: [] as FindingCandidate[] },
    { id: "unmapped", title: "Decisão indisponível", open: false, candidates: [] as FindingCandidate[] },
    { id: "pending", title: "Aguardando revisão final", open: false, candidates: [] as FindingCandidate[] },
  ];
  for (const candidate of candidates) {
    const id = candidate.blockedReason
      ? "blocked"
      : candidate.aiDecision === "kept"
        ? candidateKind(candidate) === "question"
          ? "questions"
          : "eligible"
        : candidate.aiDecision;
    required(
      groups.find((group) => group.id === id),
      `finding group ${id}`,
    ).candidates.push(candidate);
  }
  return groups.filter((group) => group.candidates.length > 0);
}

export function findingAuditSummary(view: FindingAuditView | undefined) {
  if (!view) return "Carregando auditoria de candidatos…";
  if (!view.audit) return "Histórico de descarte indisponível para esta execução.";
  const { candidates, complete } = view.audit;
  const discarded = candidates.filter((candidate) => candidate.aiDecision === "discarded").length;
  const unmapped = candidates.filter((candidate) => candidate.aiDecision === "unmapped").length;
  const pending = candidates.filter((candidate) => candidate.aiDecision === "pending").length;
  const blocked = candidates.filter((candidate) => candidate.blockedReason).length;
  return [
    `${candidates.length} candidato${candidates.length === 1 ? "" : "s"}`,
    `${discarded} descartado${discarded === 1 ? "" : "s"} pela IA`,
    blocked && `${blocked} bloqueado${blocked === 1 ? "" : "s"}`,
    unmapped && `${unmapped} ${unmapped === 1 ? "decisão indisponível" : "decisões indisponíveis"}`,
    pending && `${pending} pendente${pending === 1 ? "" : "s"}`,
    !complete && "auditoria incompleta",
  ]
    .filter(Boolean)
    .join(" · ");
}

export function findingTitle(candidate: FindingCandidate) {
  const line = candidate.body.split("\n").find((line) => POINT_MARKER.test(line));
  return (
    line
      ?.replace(/^.*?\[(Achado|Pergunta|Finding|Question)\]\s*/i, "")
      .replace(/[*_]+$/g, "")
      .trim()
      .slice(0, 180) || (candidate.origin === "body" ? "Achados gerais da revisão" : candidate.id)
  );
}

/**
 * The finding as the app shows it, under its own title: without the fingerprint and attribution
 * lines GitLab needs, and without the [Achado]/[Pergunta] line that `findingTitle` already shows.
 */
export function findingDisplayBody(body: string) {
  let titleRemoved = false;
  return body
    .split("\n")
    .filter((line) => {
      if (/^\s*<!--\s*ocr:.*-->\s*$/.test(line)) return false;
      if (
        /^\s*<sub>.*(Revisão automatizada por|Automated review by) OpenCodeReview.*<\/sub>\s*$/i.test(line) ||
        /^\s*<sub>🤖 OpenCodeReview<\/sub>\s*$/i.test(line)
      ) {
        return false;
      }
      if (!titleRemoved && POINT_MARKER.test(line)) {
        titleRemoved = true;
        return false;
      }
      return true;
    })
    .join("\n")
    .trim();
}

export function reviewBodyHasPostableFinding(body: string | null) {
  return Boolean(
    body?.split("\n").some((line) => {
      const normalized = line
        .trimStart()
        .replace(/^[*_>#-]+\s*/, "")
        .replace(/^[*_>#-]+\s*/, "");
      return POINT_MARKER_PREFIX.test(normalized);
    }),
  );
}

/** Start of the review summary section that lists lower-priority suggestions (backend `SUGGESTIONS_HEADING`):
 * a collapsed `<details>` block on GitLab, or the heading reviews used before it was collapsed. */
const SUGGESTIONS_STARTS = [
  "<details>\n<summary>💡 Sugestões",
  "### 💡 Sugestões",
  "<details>\n<summary>💡 Suggestions",
  "### 💡 Suggestions",
];

/** The suggestions section of a review body as Markdown, or "" when it has none. A collapsed block is
 * shown as a heading: the app's Markdown view renders no HTML. */
export function reviewBodySuggestions(body: string | null) {
  if (!body) return "";
  const starts = SUGGESTIONS_STARTS.map((heading) =>
    body.startsWith(heading) ? 0 : body.indexOf(`\n${heading}`),
  ).filter((start) => start >= 0);
  if (starts.length === 0) return "";
  return body
    .slice(Math.min(...starts))
    .trim()
    .replace(/^<details>\n<summary>(.*?)<\/summary>/, "### $1")
    .replace(/\n*<\/details>$/, "");
}
