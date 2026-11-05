import { ChevronDown, RotateCcw } from "lucide-react";
import { type ComponentProps, useEffect, useId, useRef, useState } from "react";
import { BrandLoading } from "../../../components/BrandLoading";
import { DSBadge, DSButton, DSEmptyState, DSError } from "../../../components/DesignSystem";
import { GitLabMark } from "../../../components/GitLabMark";
import { Markdown } from "../../../components/Markdown";
import type { ReviewCard, ReviewContextPlan, ReviewRun, UsageStats } from "../../../types";
import { ArrowTabs } from "../../components/ArrowTabs";
import { FindingSelection } from "../../components/FindingSelection";
import { ReviewDecisionCarousel } from "../../components/ReviewDecisionCarousel";
import { reviewBodyHasPostableFinding, reviewBodySuggestions, reviewPointKind } from "../../lib/findings";
import { parseChunkProgress } from "../../lib/reviewFlow";
import { ContextView, PlatformChunkProgress, RiskView, UsageView } from "./RunDetails";
import { parseComments, parseRisk, type ReviewDecision, shortSha, streamStatusLabel } from "./reviewHelpers";

export function ReviewDetail({
  card,
  run,
  viewerLogin,
  usage,
  context,
  contextError,
  loading,
  streamState,
  streamError,
  lastUpdatedAt,
  busy,
  findingSelectionReady,
  findingSelection,
  onReprocess,
  onCancel,
  onDecide,
}: {
  card: ReviewCard;
  run: ReviewRun | null;
  viewerLogin: string | null;
  usage: UsageStats | null;
  context: ReviewContextPlan | null;
  contextError: string | null;
  loading: boolean;
  streamState: string;
  streamError: string | null;
  lastUpdatedAt: number | null;
  busy: boolean;
  findingSelectionReady: boolean;
  findingSelection: ComponentProps<typeof FindingSelection>;
  onReprocess: (forceFull?: boolean) => void;
  onCancel: () => void;
  onDecide: (decision: ReviewDecision) => void;
}) {
  const [decision, setDecision] = useState<ReviewDecision | "">("");
  const [resultSection, setResultSection] = useState("findings");
  const resultPanelId = useId();
  const resultPanelRef = useRef<HTMLDivElement>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: resultSection is a trigger: scroll to the top when the result tab changes
  useEffect(() => {
    if (resultPanelRef.current) resultPanelRef.current.scrollTop = 0;
  }, [resultSection]);
  const displayRun = run;
  const comments = parseComments(displayRun?.commentsJson ?? null);
  const hasBodyFindings = reviewBodyHasPostableFinding(displayRun?.reviewBody ?? null);
  const suggestions = reviewBodySuggestions(displayRun?.reviewBody ?? null);
  const hasFindings = comments.length > 0 || hasBodyFindings;
  const hasActionableFindings =
    comments.some(
      (comment) =>
        ["finding", "mixed"].includes(reviewPointKind(comment.body)) && comment.status !== "duplicate_suppressed",
    ) || ["finding", "mixed"].includes(reviewPointKind(displayRun?.reviewBody ?? ""));
  const hasQuestions =
    comments.some((comment) => reviewPointKind(comment.body) === "question") ||
    reviewPointKind(displayRun?.reviewBody ?? "") === "question";
  const duplicateCount = comments.filter((comment) => comment.status === "duplicate_suppressed").length;
  const allFindingsDuplicated = comments.length > 0 && duplicateCount === comments.length && !hasBodyFindings;
  const canCancel = card.lane === "queued" || run?.status === "running" || run?.status === "pending";
  const canDecide = run?.status === "preview" && card.lane === "ready";
  const live = run?.status === "running" || run?.status === "pending";
  const ownPr = Boolean(viewerLogin && card.author && viewerLogin.toLowerCase() === card.author.toLowerCase());
  const chunkProgress = live ? parseChunkProgress(run?.tail ?? "") : null;
  const decisionOptions = [
    {
      value: "request_changes",
      label: "Solicitar mudanças",
      description: "Peça ao autor para corrigir os achados selecionados.",
      tone: "danger",
      enabled: hasActionableFindings && !allFindingsDuplicated && !ownPr,
    },
    {
      value: "comment",
      label: allFindingsDuplicated ? "Marcar como coberto" : "Publicar comentários",
      description: allFindingsDuplicated
        ? "Registre que outra execução já os publicou."
        : "Compartilhe os achados sem um veredito no GitLab.",
      tone: "secondary",
      enabled: hasFindings || Boolean(suggestions),
    },
    {
      value: "approve_post",
      label: "Aprovar",
      description: "Aprove este merge request no GitLab.",
      tone: "primary",
      enabled: !hasFindings && !ownPr,
    },
    {
      value: "mark_reviewed",
      label: "Marcar como revisado",
      description: "Feche localmente sem publicar no GitLab.",
      tone: "secondary",
      enabled: ownPr || !hasActionableFindings || allFindingsDuplicated,
    },
    {
      value: "skip",
      label: "Ignorar",
      description: "Deixe esta revisão sem publicar.",
      tone: "secondary",
      enabled: true,
    },
  ] as const;
  const selectedDecision = decisionOptions.find((option) => option.enabled && option.value === decision);
  const canReprocess = !["closed", "merged", "draft", "auto_skipped"].includes(card.lane);
  const risk = run ? parseRisk(run) : null;
  return (
    <>
      <div className="review-overview-toolbar flex flex-none items-center justify-between gap-3 border-b border-line bg-surface px-5 py-2.5">
        {/* The connection status only matters while the run can still change. */}
        {live ? (
          <span className="review-detail-stream-status inline-flex items-center gap-1.5 text-caption text-fg-subtle">
            <span className="size-1.5 rounded-full bg-emerald-500" />
            {streamStatusLabel(streamState)}
            {lastUpdatedAt ? ` · ${new Date(lastUpdatedAt).toLocaleTimeString()}` : ""}
          </span>
        ) : (
          <span />
        )}
        <div className="review-overview-actions flex items-center gap-2">
          {card.url && (
            <a
              className="inline-flex size-8 items-center justify-center rounded-md text-fg-muted hover:bg-surface-muted hover:text-fg"
              href={card.url}
              target="_blank"
              rel="noreferrer"
              aria-label="Abrir no GitLab"
              title="Abrir no GitLab"
            >
              <GitLabMark className="size-4" />
            </a>
          )}
          {canCancel ? (
            <DSButton tone="danger" onClick={onCancel} disabled={busy}>
              Stop
            </DSButton>
          ) : (
            canReprocess && (
              <details className="review-rerun-menu group relative">
                <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-md border border-line bg-surface px-3 text-small font-medium text-fg shadow-card hover:bg-surface-muted [&::-webkit-details-marker]:hidden">
                  <RotateCcw className="size-3.5" />
                  {run ? "Reexecutar" : "Executar revisão"}
                  <ChevronDown className="size-3.5 text-fg-subtle transition-transform group-open:rotate-180" />
                </summary>
                <div className="absolute top-full right-0 z-30 mt-1 flex w-44 flex-col gap-1 rounded-lg border border-line bg-surface p-1.5 shadow-pop">
                  <DSButton
                    disabled={busy}
                    title="Analisa de novo as mudanças novas; se nada mudou, repete o escopo anterior. Consome uso do modelo."
                    onClick={() => onReprocess(false)}
                  >
                    {run ? "Reexecutar revisão" : "Executar revisão"}
                  </DSButton>
                  {run && (
                    <DSButton disabled={busy} onClick={() => onReprocess(true)}>
                      Re-run full
                    </DSButton>
                  )}
                </div>
              </details>
            )
          )}
        </div>
      </div>
      <div className="review-result-tabs flex-none border-b border-line bg-surface px-5 py-2">
        <ArrowTabs
          tabs={[
            { id: "findings", label: "Achados" },
            { id: "details", label: "Detalhes" },
          ]}
          activeId={resultSection}
          panelId={resultPanelId}
          label="Seções do resultado"
          previousLabel="Seção anterior do resultado"
          nextLabel="Próxima seção do resultado"
          showArrows={false}
          onChange={setResultSection}
        />
      </div>
      <div
        ref={resultPanelRef}
        id={resultPanelId}
        role="tabpanel"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: WAI-ARIA tabs pattern: the panel is focusable so keyboard users can reach its content after the tab list
        tabIndex={0}
        aria-labelledby={`${resultPanelId}-tab-${resultSection}`}
        className="review-overview-body flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto bg-canvas p-5 outline-none"
      >
        {run?.errorMessage && <DSError>{run.errorMessage}</DSError>}
        {streamError && (
          <div className="review-stream-warning rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-small text-amber-900">
            {streamError} · using fallback polling
          </div>
        )}
        {card.resultError && <DSError>{card.resultError}</DSError>}
        <div
          className="review-result-section flex flex-col gap-4 [&[hidden]]:hidden"
          data-review-result-section="details"
          hidden={resultSection !== "details"}
        >
          {contextError && <DSError>{contextError}</DSError>}
          {chunkProgress && <PlatformChunkProgress progress={chunkProgress} />}
          <section className="review-run-details flex flex-col gap-3" aria-label="Detalhes da execução">
            <header className="review-run-details-heading flex items-center justify-between">
              <h3 className="text-heading text-fg">Detalhes da execução</h3>
              <span className="text-caption text-fg-subtle">
                {risk ? `Risco ${risk.score}/100 · ${risk.level}` : "Uso, contexto e resultado"}
              </span>
            </header>
            <div className="review-run-details-body flex flex-col gap-3">
              {card.resultState && (
                <div className="review-run-result-summary rounded-xl border border-line bg-surface p-4">
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="m-0 text-sm font-semibold text-fg">Resultado da execução</h2>
                    <DSBadge tone={card.resultPublicationStatus === "failed" ? "danger" : "success"} dot>
                      {card.resultState.replaceAll("_", " ")}
                    </DSBadge>
                    <span className="text-xs font-semibold text-fg-subtle">
                      {card.resultPublicationStatus ?? "só local"}
                    </span>
                  </div>
                  <p className="m-0 mt-2 text-sm text-fg-muted">
                    Observado {shortSha(card.resultObservedHeadSha)} · revisado {shortSha(card.resultReviewedHeadSha)}
                    {card.resultInherited && card.resultPriorHeadSha
                      ? ` · herdado de ${shortSha(card.resultPriorHeadSha)}`
                      : ""}
                  </p>
                  {card.resultError && <p className="m-0 mt-2 text-sm font-semibold text-danger">{card.resultError}</p>}
                </div>
              )}
              {card.isStacked && (
                <div className="review-run-stacked-summary rounded-xl border border-line bg-surface p-4">
                  <div className="flex flex-wrap items-center gap-2">
                    <h2 className="m-0 text-sm font-semibold text-fg">MR empilhado</h2>
                    <span className="rounded-full border border-line bg-surface-muted px-2.5 py-1 text-xs font-bold text-fg-muted">
                      {card.baseRef ?? "base"} → {card.headRef ?? "head"}
                    </span>
                    {card.baseSha && (
                      <span className="text-xs font-semibold text-fg-subtle">base {card.baseSha.slice(0, 8)}</span>
                    )}
                  </div>
                  <p className="m-0 mt-2 text-sm text-fg-muted">
                    A revisão usa a branch base imediata, então os comentários miram só o delta deste MR.
                  </p>
                </div>
              )}
              {run && <UsageView run={run} usage={usage} />}
              {run && <RiskView run={run} />}
              {context && <ContextView context={context} />}
            </div>
          </section>
        </div>

        <div
          className="review-result-section flex flex-col gap-4 [&[hidden]]:hidden"
          data-review-result-section="findings"
          hidden={resultSection !== "findings"}
        >
          {hasQuestions && !hasActionableFindings && (
            <p className="review-stage-note rounded-lg border border-sky-200 bg-sky-50 px-3 py-2 text-small text-sky-900">
              Só perguntas: confirmação pedida, nenhum defeito comprovado selecionado.
            </p>
          )}
          <FindingSelection {...findingSelection}>
            {loading ? (
              <BrandLoading label="Carregando revisão…" />
            ) : comments.length > 0 || displayRun?.reviewBody ? (
              <details
                className="finding-group min-w-0 rounded-xl border border-line bg-surface shadow-card"
                data-finding-group="recorded"
                open
              >
                <summary className="flex cursor-pointer list-none items-center justify-between gap-3 border-b border-line px-4 py-3 text-heading text-fg">
                  <span>Achados finais</span>
                  <span className="text-caption font-normal text-fg-subtle">
                    {comments.length} inline{hasBodyFindings ? " · revisão geral" : ""}
                    {suggestions ? " · sugestões" : ""}
                  </span>
                </summary>
                <div className="finding-group-list grid min-w-0 gap-3 flex flex-col gap-3 p-4">
                  <div className="review-comments flex flex-col gap-3">
                    {comments.map((comment, index) => (
                      <div
                        // biome-ignore lint/suspicious/noArrayIndexKey: read-only list that is never reordered; entries can repeat, so the index keeps keys unique
                        key={index}
                        className={`review-comment rounded-lg border border-line bg-surface p-4 ${comment.status === "duplicate_suppressed" ? "review-comment-duplicate opacity-70" : ""}`}
                      >
                        <span className="review-comment-loc mb-2 inline-flex rounded-md bg-surface-sunken px-1.5 py-0.5 font-mono text-[0.7857rem] text-fg-muted">
                          {comment.path}:{comment.line}
                        </span>
                        {comment.status === "duplicate_suppressed" && (
                          <div className="mb-3 rounded-lg border border-warning bg-amber-50 px-3 py-2 text-xs font-bold text-warning">
                            Já publicado por outra execução do OpenCodeReview
                            {comment.duplicateAuthor ? ` (${comment.duplicateAuthor})` : ""}
                            {comment.duplicateUrl && (
                              <>
                                {" "}
                                ·{" "}
                                <a className="underline" href={comment.duplicateUrl} target="_blank" rel="noreferrer">
                                  View on GitLab
                                </a>
                              </>
                            )}
                          </div>
                        )}
                        <Markdown>{comment.body}</Markdown>
                      </div>
                    ))}
                  </div>
                  {hasBodyFindings || comments.length === 0 ? (
                    <div className="review-comment rounded-lg border border-line bg-surface p-4">
                      <Markdown>{displayRun?.reviewBody ?? ""}</Markdown>
                    </div>
                  ) : (
                    suggestions && (
                      <div className="review-comment review-suggestions rounded-lg border border-line bg-surface p-4">
                        <Markdown>{suggestions}</Markdown>
                      </div>
                    )
                  )}
                </div>
              </details>
            ) : (
              <DSEmptyState>{live ? "Aguardando saída do Claude…" : "Nenhuma revisão gerada ainda."}</DSEmptyState>
            )}
          </FindingSelection>
        </div>
        {resultSection === "details" && live && (
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            <h2 className="m-0 mb-3 text-sm font-semibold">Log ao vivo</h2>
            <pre className="min-h-0 min-w-0 flex-1 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-surface-muted p-4 text-xs leading-relaxed text-fg-muted">
              {run?.tail || "Aguardando saída…"}
            </pre>
          </div>
        )}
      </div>
      {canDecide && (
        <div
          className="review-decision-footer flex flex-none flex-col gap-3 border-t border-line bg-surface px-5 py-3 [&[hidden]]:hidden"
          hidden={resultSection !== "findings"}
        >
          <p className="m-0 text-small text-fg-muted">
            {ownPr
              ? "Este MR é seu. O GitLab bloqueia autoaprovação e solicitar mudanças; marque como revisado localmente ou ignore."
              : allFindingsDuplicated
                ? `All ${duplicateCount} finding${duplicateCount === 1 ? "" : "s"} already posted by another OpenCodeReview run. Marcar como coberto locally.`
                : hasQuestions && !hasActionableFindings
                  ? "Só perguntas selecionadas. Publique como comentários, marque como revisado localmente ou ignore; sem solicitar mudanças."
                  : hasFindings
                    ? duplicateCount > 0
                      ? `Publique os achados novos; ${duplicateCount} duplicado${duplicateCount === 1 ? "" : "s"} será${duplicateCount === 1 ? "" : "ão"} omitido${duplicateCount === 1 ? "" : "s"}.`
                      : hasBodyFindings && comments.length === 0
                        ? "Publique os achados no GitLab como comentário da revisão, ou ignore sem publicar."
                        : "Publique os achados inline no GitLab, ou ignore sem publicar."
                    : "Nenhum achado. Aprove no GitLab, marque como revisado localmente ou ignore."}
          </p>
          <div className="review-decision-controls flex items-center gap-3">
            <ReviewDecisionCarousel
              options={decisionOptions.filter((option) => option.enabled)}
              value={selectedDecision?.value ?? ""}
              disabled={busy || !findingSelectionReady}
              onChange={(value) => setDecision(value as ReviewDecision)}
            />
            <DSButton
              className="review-decision-apply shrink-0"
              tone={selectedDecision?.value === "request_changes" ? "danger" : "primary"}
              disabled={busy || !selectedDecision || !findingSelectionReady}
              onClick={() => {
                if (selectedDecision && findingSelectionReady) onDecide(selectedDecision.value);
              }}
            >
              {selectedDecision?.label ?? "Aplicar decisão"}
            </DSButton>
          </div>
        </div>
      )}
    </>
  );
}
