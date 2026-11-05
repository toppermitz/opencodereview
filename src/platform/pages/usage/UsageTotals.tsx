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
        label="Runs"
        value={totals.runs.toLocaleString()}
        note={`on ${totals.prs.toLocaleString()} ${totals.prs === 1 ? "PR" : "PRs"}`}
      />
      <Total
        label="Findings"
        value={totals.findingsFound.toLocaleString()}
        note={`${totals.findings.toLocaleString()} posted · ${totals.questionsFound.toLocaleString()} ${totals.questionsFound === 1 ? "question" : "questions"}`}
        hint="Findings the reviews kept, published or not; posted counts the ones published to GitLab."
      />
      <Total
        label="Median time"
        value={formatFlowDuration(totals.medianDurationMs)}
        hint="From a run's first stage to its last, without time spent in the queue."
      />
      <Total
        label="Reused chunks"
        value={totals.chunks > 0 ? pct(totals.reusedChunks, totals.chunks) : "—"}
        note={totals.chunks > 0 && `${totals.reusedChunks.toLocaleString()} of ${totals.chunks.toLocaleString()}`}
        hint="Chunks whose earlier review result was reused instead of running the AI again. Not the provider's prompt cache."
      />
      <Total
        label="Re-review overhead"
        value={compactTokens(totals.overheadTokens)}
        note={pct(totals.overheadTokens, totals.tokens)}
        hint="Tokens spent on runs that were superseded by new commits, failed or were cancelled."
        className={overheadShare > 0.25 ? "text-warning" : undefined}
      />
    </dl>
  );
}
