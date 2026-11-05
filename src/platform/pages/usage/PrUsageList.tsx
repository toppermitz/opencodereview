import { keepPreviousData, useInfiniteQuery } from "@tanstack/react-query";
import { ChevronRight } from "lucide-react";
import { useState } from "react";
import { BrandLoading } from "../../../components/BrandLoading";
import { cx, DSButton, DSEmptyState, DSError } from "../../../components/DesignSystem";
import { Tip } from "../../../components/Tip";
import { setupApi } from "../../../services/setupApi";
import type { UsageQuery } from "../../../types";
import { formatFlowDuration } from "../../lib/reviewFlow";
import { toggleSet } from "../../lib/utils";
import { compactTokens, pct, usageError } from "./format";
import { PrRunsTimeline } from "./PrRunsTimeline";
import { RunStrip } from "./RunStrip";

const PAGE_SIZE = 25;
const columns =
  "grid grid-cols-[minmax(0,1fr)_5rem] items-center gap-x-4 md:grid-cols-[minmax(0,1fr)_10rem_5rem_12rem_8rem_6rem]";

/** Findings the review kept, with how many were published when that differs. */
export function FindingsCell({ found, posted }: { found: number; posted: number }) {
  if (found === 0 && posted === 0) return <span className="text-fg-subtle">–</span>;
  return (
    <Tip content={`${found} found by the review · ${posted} posted to GitLab`} className="justify-end">
      <span className="text-fg">{found.toLocaleString()}</span>
      {posted !== found && <span className="whitespace-pre text-fg-subtle"> · {posted.toLocaleString()} posted</span>}
    </Tip>
  );
}

/** Share of the period's tokens, as a bar and a percentage. */
export function ShareBar({ tokens, total }: { tokens: number; total: number }) {
  return (
    <span className="flex items-center gap-2">
      <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-sunken">
        <span
          className="block h-full rounded-full bg-brand"
          style={{ width: `${total > 0 ? Math.min(100, (tokens / total) * 100) : 0}%` }}
        />
      </span>
      <span className="w-11 text-right font-mono text-caption text-fg-muted">{pct(tokens, total, 1)}</span>
    </span>
  );
}

export function PrUsageList({ query, totalTokens, live }: { query: UsageQuery; totalTokens: number; live: boolean }) {
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const prs = useInfiniteQuery({
    queryKey: ["usage", "prs", query.days, query.repoId, query.tz],
    queryFn: ({ pageParam }) => setupApi.usagePrs(query, pageParam, PAGE_SIZE),
    initialPageParam: 0,
    getNextPageParam: (page) => page.nextOffset ?? undefined,
    placeholderData: keepPreviousData,
    refetchInterval: live ? 5000 : false,
  });
  if (prs.isError && !prs.data) return <DSError>{usageError(prs.error)}</DSError>;
  if (!prs.data) return <BrandLoading size="compact" label="Loading merge requests…" />;
  const rows = prs.data.pages.flatMap((page) => page.items);
  if (!rows.length) return <DSEmptyState>No merge requests ran a review in this period.</DSEmptyState>;
  const total = prs.data.pages.at(-1)?.total ?? rows.length;
  return (
    <div className="flex flex-col">
      <div className={cx(columns, "border-b border-line px-2 pb-2 text-caption text-fg-subtle")} aria-hidden="true">
        <span>Merge request</span>
        <span className="max-md:hidden">Runs</span>
        <span className="text-right">Tokens</span>
        <span className="max-md:hidden">Share</span>
        <span className="text-right max-md:hidden">Findings</span>
        <span className="text-right max-md:hidden">Median time</span>
      </div>
      <ul className="flex flex-col">
        {rows.map((pr) => {
          const open = expanded.has(pr.prId);
          const panelId = `usage-pr-${pr.prId}-runs`;
          return (
            <li key={pr.prId} className="border-b border-line">
              <button
                type="button"
                aria-expanded={open}
                aria-controls={panelId}
                onClick={() => setExpanded((current) => toggleSet(current, pr.prId))}
                className={cx(
                  columns,
                  "w-full cursor-pointer rounded-md px-2 py-2.5 text-left text-small transition-colors hover:bg-surface-muted focus-visible:outline-2 focus-visible:outline-brand",
                )}
              >
                <span className="flex min-w-0 items-center gap-2">
                  <ChevronRight
                    className={cx("size-4 shrink-0 text-fg-subtle transition-transform", open && "rotate-90")}
                    aria-hidden="true"
                  />
                  <span className="shrink-0 text-fg-muted">
                    {pr.repo} #{pr.number}
                  </span>
                  <span className="truncate text-fg">{pr.title}</span>
                </span>
                <span className="flex items-center gap-2 max-md:hidden">
                  <span className="w-5 text-right font-mono text-caption text-fg-muted">{pr.runs}</span>
                  <RunStrip runs={pr.strip} className="flex-1" />
                </span>
                <span className="text-right font-mono text-fg tabular-nums">{compactTokens(pr.tokens)}</span>
                <span className="max-md:hidden">
                  <ShareBar tokens={pr.tokens} total={totalTokens} />
                </span>
                <span className="text-right font-mono text-caption max-md:hidden">
                  <FindingsCell found={pr.findingsFound} posted={pr.findings} />
                </span>
                <span className="text-right font-mono text-fg-muted max-md:hidden">
                  {formatFlowDuration(pr.medianDurationMs)}
                </span>
              </button>
              {open && (
                <div id={panelId} className="pt-1 pr-2 pb-3 pl-8">
                  <PrRunsTimeline prId={pr.prId} days={query.days} tz={query.tz} />
                </div>
              )}
            </li>
          );
        })}
      </ul>
      <div className="flex items-center justify-between gap-3 pt-3 text-caption text-fg-subtle">
        <span className="tabular-nums">
          {rows.length.toLocaleString()} of {total.toLocaleString()} merge requests
        </span>
        {prs.hasNextPage && (
          <DSButton size="sm" onClick={() => void prs.fetchNextPage()} disabled={prs.isFetchingNextPage}>
            {prs.isFetchingNextPage ? "Loading…" : "Show more"}
          </DSButton>
        )}
      </div>
    </div>
  );
}
