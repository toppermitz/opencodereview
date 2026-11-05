import { Lock } from "lucide-react";
import { type ReactNode, useId, useMemo, useState } from "react";
import { BrandLoading } from "../../components/BrandLoading";
import { DSButton, DSEmptyState, DSError } from "../../components/DesignSystem";
import { Markdown } from "../../components/Markdown";
import type { FindingAuditView } from "../../types";
import {
  candidateKind,
  findingAuditSummary,
  findingDisplayBody,
  findingTitle,
  groupFindingCandidates,
} from "../lib/findings";

export function FindingSelection({
  view,
  selectedIds,
  loading,
  busy,
  dirty,
  conflict,
  error,
  onToggle,
  onSave,
  onReset,
  children,
}: {
  view: FindingAuditView | undefined;
  selectedIds: string[];
  loading: boolean;
  busy: boolean;
  dirty: boolean;
  conflict: boolean;
  error: string | null;
  children?: ReactNode;
  onToggle: (id: string) => void;
  onSave: () => void;
  onReset: () => void;
}) {
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  const candidates = view?.audit?.candidates ?? [];
  const groups = groupFindingCandidates(candidates);
  const [tabId, setTabId] = useState("eligible");
  const panelId = useId();
  const group = groups.find((group) => group.id === tabId) ?? groups[0];
  const selectedOf = (kind: "finding" | "question") =>
    candidates.filter((candidate) => selected.has(candidate.id) && candidateKind(candidate) === kind).length;
  const editable = Boolean(view?.editable && !busy && !conflict && !error);
  return (
    <section className="finding-selection grid min-w-0 gap-3" aria-label="Achados da revisão">
      <div className="finding-selection-intro grid gap-1.5 text-small text-fg-muted">
        <div className="finding-review-heading flex flex-wrap items-center justify-between gap-2 text-caption text-fg-subtle">
          <span>{error ? "Não foi possível carregar a auditoria de candidatos." : findingAuditSummary(view)}</span>
          {view?.audit &&
            (view.editable ? (
              <span className="tabular-nums">
                {selectedOf("finding")} achado{selectedOf("finding") === 1 ? "" : "s"} · {selectedOf("question")}{" "}
                pergunta{selectedOf("question") === 1 ? "" : "s"} selecionada{selectedOf("question") === 1 ? "" : "s"}
              </span>
            ) : (
              <span
                className="inline-flex items-center gap-1"
                title={!view.audit.complete ? "Auditoria incompleta. A seleção libera após a validação." : undefined}
              >
                <Lock className="size-3" aria-hidden="true" />
                {!view.audit.complete ? "Auditoria incompleta" : "Somente leitura"}
              </span>
            ))}
        </div>
        {candidates.some((candidate) => candidateKind(candidate) === "question") && (
          <p>
            Perguntas precisam de confirmação, não de uma correção presumida. São opcionais e sozinhas não justificam
            Solicitar mudanças.
          </p>
        )}
        {view?.editable && candidates.length > 0 && (
          <p>Escolha os achados abaixo, confirme-os e depois escolha como publicar a revisão.</p>
        )}
      </div>
      {error && <DSError>{error}</DSError>}
      {conflict && (
        <DSError>
          A seleção salva mudou em outro lugar. Recarregue antes de salvar; seu rascunho não foi enviado.
        </DSError>
      )}
      <div className="finding-selection-list grid min-w-0 gap-3">
        {view?.audit ? (
          <>
            {candidates.length === 0 && (
              <DSEmptyState>
                {view.editable
                  ? "Nenhum candidato a achado foi gerado. Escolha uma decisão de revisão abaixo."
                  : "Nenhum candidato a achado registrado nesta execução."}
              </DSEmptyState>
            )}
            {group && (
              <>
                {groups.length > 1 && (
                  <fieldset
                    className="m-0 min-w-0 border-0 p-0 finding-groups flex flex-wrap gap-1.5"
                    aria-label="Grupos de achados"
                  >
                    {groups.map((item) => (
                      <button
                        key={item.id}
                        type="button"
                        className={`finding-group-chip inline-flex h-7 cursor-pointer items-center gap-1.5 rounded-full border px-2.5 text-caption font-medium transition-colors ${item.id === group.id ? "border-line-strong bg-surface-muted text-fg" : "border-line text-fg-muted hover:text-fg"}`}
                        aria-pressed={item.id === group.id}
                        aria-controls={panelId}
                        onClick={() => setTabId(item.id)}
                      >
                        {item.title}
                        <span className="text-fg-subtle tabular-nums">{item.candidates.length}</span>
                      </button>
                    ))}
                  </fieldset>
                )}
                <div id={panelId} className="finding-tab-panel grid min-w-0 gap-3" data-finding-group={group.id}>
                  <div className="finding-group-list grid min-w-0 gap-3">
                    {group.candidates.map((candidate) => {
                      const override = selected.has(candidate.id) !== candidate.defaultSelected;
                      const aiTag =
                        candidate.aiDecision === "kept"
                          ? null
                          : candidate.aiDecision === "unmapped"
                            ? "IA: decisão indisponível"
                            : `IA: ${candidate.aiDecision}`;
                      return (
                        <article
                          key={candidate.id}
                          className={`finding-candidate grid min-w-0 gap-3 rounded-xl border border-line bg-surface p-4 pb-1 shadow-card transition-colors ${selected.has(candidate.id) ? "finding-candidate-selected border-brand/50 ring-1 ring-brand/20" : ""}`}
                        >
                          <label className="finding-candidate-choice flex cursor-pointer items-start gap-2.5 [&_input]:mt-0.5 [&_input]:size-4 [&_input]:flex-none [&_input]:accent-brand">
                            <input
                              type="checkbox"
                              aria-label={`Include ${candidateKind(candidate) === "question" ? "question" : "finding"} ${candidate.id}`}
                              checked={selected.has(candidate.id)}
                              disabled={!editable || Boolean(candidate.blockedReason)}
                              onChange={() => onToggle(candidate.id)}
                            />
                            <span className="min-w-0">
                              <span className="finding-candidate-title block text-small font-semibold text-fg [overflow-wrap:anywhere]">
                                {findingTitle(candidate)}
                              </span>
                              <span className="finding-candidate-location mt-1.5 inline-block max-w-full rounded-md bg-surface-muted px-1.5 py-0.5 font-mono text-caption text-fg-muted [overflow-wrap:anywhere]">
                                {candidate.comment
                                  ? `${candidate.comment.path ?? "Caminho desconhecido"}:${candidate.comment.line ?? "?"}`
                                  : candidateKind(candidate) === "question"
                                    ? "Pergunta geral da revisão"
                                    : "Achados gerais no corpo da revisão"}
                                {candidate.chunkId !== null && ` · Chunk ${candidate.chunkId}`}
                              </span>
                            </span>
                          </label>
                          {(aiTag || override) && (
                            <div className="finding-candidate-tags flex flex-wrap gap-1.5 pl-6.5 [&_span]:rounded-full [&_span]:bg-surface-muted [&_span]:px-2 [&_span]:py-0.5 [&_span]:text-caption [&_span]:font-medium [&_span]:text-fg-muted">
                              {aiTag && <span>{aiTag}</span>}
                              {override && <span>Sobrescrita manual</span>}
                            </div>
                          )}
                          {group.id !== "eligible" && (
                            <p className="finding-candidate-reason pl-6.5 text-small text-fg-muted [overflow-wrap:anywhere]">
                              {candidate.aiReason ??
                                (candidate.origin === "inherited"
                                  ? "Nenhuma nova passagem da IA; veio do contexto de revisão anterior."
                                  : candidate.aiDecision === "pending"
                                    ? "Aguardando a decisão final de integração."
                                    : candidate.aiDecision === "unmapped"
                                      ? "Nenhum mapeamento confiável foi registrado. Este candidato pode ter sido reescrito ou omitido; nenhum motivo de descarte é inferido."
                                      : "Motivo da IA não registrado.")}
                            </p>
                          )}
                          {candidate.blockedReason && (
                            <p className="finding-candidate-blocked pl-6.5 text-small text-danger">
                              Não pode incluir: {candidate.blockedReason}
                            </p>
                          )}
                          <div className="finding-candidate-body min-w-0 pl-6.5 [overflow-wrap:anywhere]">
                            <Markdown>{findingDisplayBody(candidate.body)}</Markdown>
                            {group.id === "eligible" && (
                              <details className="finding-candidate-text disclosure text-small">
                                <summary>Por que a IA manteve</summary>
                                <p className="finding-candidate-reason text-small text-fg-muted [overflow-wrap:anywhere]">
                                  {candidate.aiReason ?? "AI reason not recorded."}
                                </p>
                              </details>
                            )}
                            {candidate.originalBody && (
                              <details className="finding-candidate-text disclosure text-small">
                                <summary>Achado original do chunk</summary>
                                <Markdown>{candidate.originalBody}</Markdown>
                              </details>
                            )}
                          </div>
                        </article>
                      );
                    })}
                  </div>
                </div>
              </>
            )}
            {view.audit.originalBody && (
              <details className="finding-original-review disclosure text-small">
                <summary>Resumo original da IA</summary>
                <Markdown>{view.audit.originalBody}</Markdown>
              </details>
            )}
          </>
        ) : (
          <>
            {children}
            {loading && !error && <BrandLoading size="compact" label="Carregando auditoria de candidatos…" />}
            {view && !loading && (
              <details className="finding-group min-w-0" data-finding-group="unavailable">
                <summary>
                  <span>Achados descartados</span>
                  <span>Histórico indisponível</span>
                </summary>
                <p className="finding-candidate-reason text-small text-fg-muted [overflow-wrap:anywhere]">
                  Nenhuma auditoria de candidatos foi registrada nesta execução. Motivos históricos de descarte não
                  podem ser reconstruídos de forma confiável; isso não significa que zero candidatos foram descartados.
                </p>
              </details>
            )}
          </>
        )}
      </div>
      {view?.editable && candidates.length > 0 && (
        <footer className="finding-selection-actions sticky -bottom-5 z-1 flex flex-wrap items-center justify-end gap-2 border-t border-line bg-surface py-3 [&>span]:min-w-30 [&>span]:flex-1 [&>span]:text-caption [&>span]:text-fg-subtle">
          <span>
            {dirty
              ? "Escolhas de achados não salvas"
              : view.selectionSaved
                ? "Achados confirmados"
                : "Confirme os achados antes de escolher uma ação"}
          </span>
          <DSButton disabled={busy} onClick={onReset}>
            {conflict ? "Recarregar seleção" : "Restaurar seleção da IA"}
          </DSButton>
          <DSButton tone="primary" disabled={!editable || (view.selectionSaved && !dirty)} onClick={onSave}>
            {busy ? "Confirmando…" : "Confirmar achados"}
          </DSButton>
        </footer>
      )}
    </section>
  );
}
