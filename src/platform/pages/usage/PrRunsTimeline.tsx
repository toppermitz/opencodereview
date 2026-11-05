import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { BrandLoading } from "../../../components/BrandLoading";
import { cx, DSBadge, DSError } from "../../../components/DesignSystem";
import { Tip } from "../../../components/Tip";
import { setupApi } from "../../../services/setupApi";
import type { UsageOutcome, UsageRange, UsageRun } from "../../../types";
import { formatFlowDuration } from "../../lib/reviewFlow";
import { shortSha } from "../reviews/reviewHelpers";
import { compactTokens, outcomeMeta, pct, usageError } from "./format";
import { FindingsCell } from "./PrUsageList";

const SUMMARY_ORDER: UsageOutcome[] = ["superseded", "failed", "cancelled", "skipped", "running", "preview"];
const runTime = (ms: number) =>
  new Date(ms).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const who = (run: UsageRun) => [...run.accounts, ...run.models].join(" · ");

/** The most common account and model, so each run only names them when it differs. */
function usualWho(runs: UsageRun[]) {
  const counts = new Map<string, number>();
  for (const run of runs) if (who(run)) counts.set(who(run), (counts.get(who(run)) ?? 0) + 1);
  return [...counts].sort((a, b) => b[1] - a[1])[0]?.[0] ?? "";
}

function runSummary(runs: UsageRun[]) {
  const total = runs.reduce((sum, run) => sum + run.tokens, 0);
  const parts = [`${runs.length} ${runs.length === 1 ? "execução" : "execuções"} neste período`];
  for (const outcome of SUMMARY_ORDER) {
    const matching = runs.filter((run) => run.outcome === outcome);
    if (!matching.length) continue;
    const label = outcomeMeta[outcome].label.toLowerCase();
    if (outcome !== "superseded") parts.push(`${matching.length} ${label}`);
    else {
      const tokens = matching.reduce((sum, run) => sum + run.tokens, 0);
      parts.push(
        `${matching.length} substituído${matching.length === 1 ? "" : "s"} por commits novos (${pct(tokens, total)} dos tokens)`,
      );
    }
  }
  return parts.join(" · ");
}

export function PrRunsTimeline({ prId, days, tz }: { prId: number; days: UsageRange; tz: number }) {
  const runs = useQuery({
    queryKey: ["usage", "pr", prId, days, tz],
    queryFn: () => setupApi.usagePrRuns(prId, { days, tz }),
  });
  if (runs.isError) return <DSError>{usageError(runs.error)}</DSError>;
  if (!runs.data) return <BrandLoading size="compact" label="Carregando execuções…" />;
  const usual = usualWho(runs.data);
  const peak = Math.max(1, ...runs.data.map((run) => run.tokens));
  return (
    <div className="flex flex-col gap-1">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-caption text-fg-muted">
        <span>{runSummary(runs.data)}</span>
        <Link
          to="/platform/reviews"
          search={{ pr: prId }}
          className="font-medium text-fg no-underline underline-offset-2 hover:underline"
        >
          Abrir revisão →
        </Link>
      </div>
      <ol className="flex flex-col" aria-label="Execuções, da mais nova">
        {runs.data.map((run) => {
          const meta = outcomeMeta[run.outcome];
          return (
            <li
              key={run.id}
              data-usage-reported={run.usageReported}
              className={cx(
                "grid grid-cols-[minmax(0,1fr)_auto_5rem] items-center gap-x-3 rounded-md py-1.5 text-caption md:grid-cols-[7.5rem_5.5rem_7rem_minmax(0,1fr)_4.5rem_8rem_7rem]",
                !run.usageReported && "opacity-50",
              )}
            >
              <span className="text-fg-muted tabular-nums max-md:hidden">{runTime(run.startedAt)}</span>
              <span className="font-mono text-fg-muted">{shortSha(run.headSha)}</span>
              <span className="max-md:order-first">
                <DSBadge tone={meta.tone} dot>
                  {meta.label}
                </DSBadge>
              </span>
              <span className="truncate text-fg-subtle max-md:hidden">{who(run) !== usual ? who(run) : ""}</span>
              <span className="text-right font-mono text-fg-muted max-md:hidden">
                {formatFlowDuration(run.durationMs)}
              </span>
              <span className="flex items-center justify-end gap-2 font-mono text-fg tabular-nums">
                {run.usageReported ? (
                  compactTokens(run.tokens)
                ) : (
                  <Tip content="This run finished without the AI CLI reporting token usage.">—</Tip>
                )}
                <span className="h-1.5 w-12 overflow-hidden rounded-full bg-surface-sunken max-md:hidden">
                  {run.usageReported && (
                    <span
                      className="block h-full rounded-full bg-brand"
                      style={{ width: `${(run.tokens / peak) * 100}%` }}
                    />
                  )}
                </span>
              </span>
              <span className="text-right font-mono max-md:hidden">
                <FindingsCell found={run.findingsFound} posted={run.findings} />
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
