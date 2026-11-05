import { cx } from "../../../components/DesignSystem";
import { Tip } from "../../../components/Tip";
import type { UsageStripRun } from "../../../types";
import { compactTokens, outcomeMeta } from "./format";

/** One segment per run, oldest first, as wide as its share of the PR's tokens and colored by outcome. */
export function RunStrip({ runs, className }: { runs: UsageStripRun[]; className?: string }) {
  const total = runs.reduce((sum, run) => sum + run.tokens, 0);
  // Runs that used (almost) nothing keep a sliver so every run stays visible.
  const floor = Math.max(total * 0.04, 1);
  const counts = Object.entries(
    runs.reduce<Record<string, number>>((all, run) => {
      const label = outcomeMeta[run.outcome].label.toLowerCase();
      all[label] = (all[label] ?? 0) + 1;
      return all;
    }, {}),
  ).map(([label, count]) => `${count} ${label}`);
  const strip = (
    <span
      role="img"
      aria-label={`${runs.length} ${runs.length === 1 ? "execução" : "execuções"}: ${counts.join(", ")}`}
      className="flex h-2 w-full gap-px overflow-hidden rounded-sm"
    >
      {runs.map((run) => (
        <span
          key={run.runId}
          className={cx("min-w-0.5", outcomeMeta[run.outcome].segment)}
          style={{ flexGrow: Math.max(run.tokens, floor), flexBasis: 0 }}
        />
      ))}
    </span>
  );
  return (
    <Tip
      className={cx("block", className)}
      content={
        <ol className="flex flex-col gap-0.5">
          {runs.map((run, index) => (
            <li key={run.runId} className="flex justify-between gap-4 tabular-nums">
              <span>
                Execução {index + 1} · {outcomeMeta[run.outcome].label}
              </span>
              <span className="font-mono text-fg-muted">{compactTokens(run.tokens)}</span>
            </li>
          ))}
        </ol>
      }
    >
      {strip}
    </Tip>
  );
}
