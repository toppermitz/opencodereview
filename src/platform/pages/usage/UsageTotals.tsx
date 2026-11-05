import type { ReactNode } from "react";
import { cx } from "../../../components/DesignSystem";
import { Hint } from "../../../components/Tip";
import type { UsageTotals as Totals } from "../../../types";
import { formatFlowDuration } from "../../lib/reviewFlow";
import { compactTokens, pct } from "./format";

function Total({
  label,
  value,
  note,
  hint,
  className,
}: {
  label: string;
  value: ReactNode;
  note?: ReactNode;
  hint?: string;
  className?: string;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="inline-flex items-center gap-1.5 text-small text-fg-subtle">
        {label}
        {hint && <Hint text={hint} />}
      </dt>
      <dd className={cx("text-title tabular-nums", className ?? "text-fg")}>{value}</dd>
      {note && <dd className="text-caption text-fg-subtle tabular-nums">{note}</dd>}
    </div>
  );
}

export function UsageTotals({ totals }: { totals: Totals }) {
  const overheadShare = totals.tokens > 0 ? totals.overheadTokens / totals.tokens : 0;
  return (
    <dl className="grid grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3 lg:grid-cols-5">
      <Total
        label="Execuções"
        value={totals.runs.toLocaleString()}
        note={`em ${totals.prs.toLocaleString()} ${totals.prs === 1 ? "MR" : "MRs"}`}
      />
      <Total
        label="Achados"
        value={totals.findingsFound.toLocaleString()}
        note={`${totals.findings.toLocaleString()} publicados · ${totals.questionsFound.toLocaleString()} ${totals.questionsFound === 1 ? "pergunta" : "perguntas"}`}
        hint="Achados que as revisões mantiveram, publicados ou não; publicados conta os que foram ao GitLab."
      />
      <Total
        label="Tempo mediano"
        value={formatFlowDuration(totals.medianDurationMs)}
        hint="Da primeira etapa da execução até a última, sem o tempo na fila."
      />
      <Total
        label="Chunks reutilizados"
        value={totals.chunks > 0 ? pct(totals.reusedChunks, totals.chunks) : "—"}
        note={totals.chunks > 0 && `${totals.reusedChunks.toLocaleString()} de ${totals.chunks.toLocaleString()}`}
        hint="Chunks cujo resultado anterior foi reutilizado em vez de rodar a IA de novo. Não é o cache de prompt do provedor."
      />
      <Total
        label="Custo de re-revisão"
        value={compactTokens(totals.overheadTokens)}
        note={pct(totals.overheadTokens, totals.tokens)}
        hint="Tokens gastos em execuções substituídas por commits novos, que falharam ou foram canceladas."
        className={overheadShare > 0.25 ? "text-warning" : undefined}
      />
    </dl>
  );
}
