import type { ReviewContextPlan, ReviewRun, UsageStats } from "../../../types";
import { type ChunkProgress, formatFlowTokens, quoteShellArgument, totalTokens } from "../../lib/reviewFlow";
import { durationLabel, fmtTokens, parseRisk, shortSha } from "./reviewHelpers";

export function PlatformChunkProgress({ progress }: { progress: ChunkProgress }) {
  const pct = Math.round((progress.done / Math.max(1, progress.total)) * 100);
  return (
    <div className="rounded-lg border border-line bg-surface p-4">
      <div className="flex items-end justify-between gap-4">
        <div>
          <div className="text-sm font-semibold text-fg">
            {progress.done}/{progress.total} chunks concluídos
          </div>
          <div className="mt-1 text-xs font-semibold text-fg-muted">
            {progress.running} em execução · {pct}% concluído
          </div>
        </div>
      </div>
      <div className="mt-3 h-2 overflow-hidden rounded-full bg-surface-muted">
        <span className="block h-full rounded-full bg-brand" style={{ width: `${pct}%` }} />
      </div>
      <div className="mt-3 flex flex-wrap gap-2">
        {progress.chunks.map((chunk) => (
          <span
            key={chunk.id}
            className={`rounded-full border px-2.5 py-1 text-xs font-semibold ${chunk.status === "done" ? "border-[color-mix(in_srgb,var(--success)_55%,var(--border))] text-success" : chunk.status === "running" ? "border-[color-mix(in_srgb,var(--accent)_55%,var(--border))] text-brand" : "border-line text-fg-subtle"}`}
          >
            {chunk.id} {chunk.status}
            {chunk.duration ? ` · ${chunk.duration}` : ""}
          </span>
        ))}
      </div>
    </div>
  );
}

export function UsageView({ run, usage }: { run: ReviewRun; usage: UsageStats | null }) {
  const hasRunUsage = run.inputTokens > 0 || run.outputTokens > 0 || run.cacheReadTokens > 0;
  if (!hasRunUsage && !usage?.runCount) return null;
  return (
    <div className="review-run-usage rounded-lg border border-line bg-surface p-4">
      <div className="flex items-end justify-between gap-4">
        <div>
          <div className="text-2xl font-semibold text-fg">{formatFlowTokens(totalTokens(run))}</div>
          <div className="text-xs font-semibold text-fg-muted">
            esta execução · {durationLabel(run) ?? "tempo n/d"}
            {run.usageModel ? ` · ${run.usageModel}` : ""}
          </div>
        </div>
        <div className="text-right text-xs font-semibold text-fg-muted">{tokenSplit(run)}</div>
      </div>
      {usage && usage.runCount > 1 && (
        <div className="mt-3 flex justify-between border-t border-line pt-3 text-xs font-semibold text-fg-muted">
          <span>
            Total do MR ({usage.runCount} execuções) · <b className="text-fg">{formatFlowTokens(totalTokens(usage))}</b>
          </span>
          <span>{tokenSplit(usage)}</span>
        </div>
      )}
    </div>
  );
}

function tokenSplit(usage: Pick<UsageStats, "inputTokens" | "outputTokens" | "cacheReadTokens">) {
  return [
    `entrada ${fmtTokens(usage.inputTokens)}`,
    `saída ${fmtTokens(usage.outputTokens)}`,
    ...(usage.cacheReadTokens > 0 ? [`leituras de cache ${fmtTokens(usage.cacheReadTokens)}`] : []),
  ].join(" · ");
}

export function RiskView({ run }: { run: ReviewRun }) {
  const risk = parseRisk(run);
  if (!risk) return null;
  return (
    <details className={`review-run-risk review-risk-${risk.level} rounded-lg border bg-surface p-4`}>
      <summary className="cursor-pointer list-none">
        <div className="flex items-start justify-between gap-3">
          <div>
            <span className="text-2xl font-semibold text-fg">{risk.score}</span>
            <span className="ml-1 text-xs font-semibold text-fg-muted">/100 Risco · {risk.level}</span>
            <p className="mt-2 text-sm text-fg-muted">{risk.summary}</p>
          </div>
          <div className="flex items-center gap-3">
            <span className="rounded-full bg-emerald-50 px-3 py-1 text-xs font-semibold text-success">
              {risk.autoMergeEligible ? "Candidato a auto-merge" : "Decisão humana"}
            </span>
            <span className="text-xs font-semibold text-fg-subtle">Detalhes ›</span>
          </div>
        </div>
      </summary>
      {risk.drivers.length > 0 && (
        <div className="mt-3 divide-y divide-line">
          {risk.drivers.map((driver) => (
            <div key={`${driver.label}-${driver.detail}`} className="flex justify-between gap-3 py-2 text-xs">
              <span className="font-semibold text-fg">{driver.label}</span>
              <span className="text-fg-muted">
                {driver.points > 0 ? "+" : ""}
                {driver.points} · {driver.detail}
              </span>
            </div>
          ))}
        </div>
      )}
    </details>
  );
}

export function ContextView({ context }: { context: ReviewContextPlan }) {
  const changed = context.changedFiles.length;
  const reprocess = context.scopeSelection?.mode === "reprocess";
  const contentReuse = context.scopeSelection?.mode === "content_reuse";
  const baseSync = context.scopeSelection?.mode === "base_sync" || contentReuse;
  const excluded = context.scopeSelection?.excludedFiles?.length ?? 0;
  const fileWord = changed === 1 ? "arquivo" : "arquivos";
  const summary =
    (reprocess
      ? `Reexecução explícita · ${changed} ${fileWord} reanalisados sem resultados em cache`
      : baseSync
        ? `${contentReuse ? "Reuso verificado por conteúdo" : "Sincronização da base"} · ${changed} ${fileWord} revisados · ${context.scopeSelection?.reusedFiles.length ?? 0} reutilizados`
        : context.incremental
          ? `Revisados só os ${changed} ${fileWord} alterados desde a última revisão`
          : `Revisão completa · ${changed} ${fileWord} alterados`) +
    (excluded ? ` · ${excluded} excluídos, não revisados` : "");
  return (
    <details className="context-audit rounded-lg border border-line bg-surface px-4 py-3">
      <summary className="cursor-pointer list-none text-sm font-semibold text-fg-muted">
        <span
          className={`mr-3 rounded-full px-3 py-1 text-xs font-semibold ${context.incremental ? "bg-emerald-50 text-success" : "bg-indigo-50 text-brand"}`}
        >
          {reprocess
            ? "Reexecução"
            : contentReuse
              ? "Reuso de conteúdo"
              : baseSync
                ? "Sync da base"
                : context.incremental
                  ? "Incremental"
                  : "MR completo"}
        </span>
        {summary}
        <span className="float-right text-fg-subtle">›</span>
      </summary>
      <div className="mt-3 grid gap-3 text-xs text-fg-muted">
        <p>
          {baseSync || reprocess ? (
            context.scopeReason
          ) : context.incremental ? (
            <>
              Só mudanças desde <code>{shortSha(context.priorSha)}</code> → <code>{shortSha(context.headSha)}</code>{" "}
              foram revisadas; código inalterado foi pulado.
            </>
          ) : (
            <>
              MR completo vs base: <code>{shortSha(context.baseSha)}</code> → <code>{shortSha(context.headSha)}</code>.
            </>
          )}
        </p>
        {context.range && (!baseSync || changed > 0) && (
          <code className="context-range rounded bg-surface-muted p-2">
            git diff {quoteShellArgument(context.range)}
            {(baseSync || reprocess) && ` -- ${context.changedFiles.map(quoteShellArgument).join(" ")}`}
          </code>
        )}
        <div className="flex flex-wrap gap-3">
          <span>
            <b>{changed}</b> alterados
          </span>
          <span>
            <b>{context.relatedFiles.length}</b> relacionados
          </span>
          <span>
            <b>{context.testFiles.length}</b> testes
          </span>
          {context.omittedCount > 0 && <span>+{context.omittedCount} omitidos</span>}
        </div>
        {changed > 0 && (
          <ul className="grid max-h-36 gap-1 overflow-auto pl-5">
            {context.changedFiles.map((file) => (
              <li key={file}>{file}</li>
            ))}
          </ul>
        )}
      </div>
    </details>
  );
}
