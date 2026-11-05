import { useState } from "react";
import { DSEmptyState } from "../../../components/DesignSystem";
import { Hint } from "../../../components/Tip";
import type { UsageGroupRow, UsageQuery, UsageReport } from "../../../types";
import { ProviderGlyph } from "../../components/AccountsPanel";
import { ArrowTabs } from "../../components/ArrowTabs";
import { formatFlowDuration } from "../../lib/reviewFlow";
import { attributedRows, compactTokens, isProvider, UNATTRIBUTED_HINT } from "./format";
import { FindingsCell, PrUsageList, ShareBar } from "./PrUsageList";

type TabId = "prs" | "repos" | "models" | "accounts";
const PANEL_ID = "usage-breakdown";
const th = "px-2 pb-2 text-caption font-normal text-fg-subtle";
const num = "px-2 py-2.5 text-right font-mono text-fg-muted";

function RowName({ row }: { row: UsageGroupRow }) {
  if (row.key === "unattributed")
    return (
      <span className="inline-flex items-center gap-1.5 text-fg-muted">
        Unattributed
        <Hint text={UNATTRIBUTED_HINT} />
      </span>
    );
  return (
    <span className="inline-flex min-w-0 items-center gap-2">
      {isProvider(row.provider) && <ProviderGlyph provider={row.provider} />}
      <span className="truncate text-fg">{row.label}</span>
    </span>
  );
}

function GroupTable({
  rows,
  total,
  kind,
}: {
  rows: UsageGroupRow[];
  total: number;
  kind: "repos" | "models" | "accounts";
}) {
  if (!rows.length) return <DSEmptyState>No runs in this period. Run a review from Reviews to fill it.</DSEmptyState>;
  const repos = kind === "repos";
  return (
    <table className="w-full table-fixed border-collapse text-small">
      <thead>
        <tr className="border-b border-line text-left">
          <th className={th}>{repos ? "Repository" : kind === "models" ? "Model" : "Account"}</th>
          <th className={`${th} w-24 text-right`}>Tokens</th>
          <th className={`${th} w-56 max-md:hidden`}>Share</th>
          {!repos && <th className={`${th} w-28 text-right max-md:hidden`}>Cache reads</th>}
          <th className={`${th} w-16 text-right max-md:hidden`}>Runs</th>
          {repos && <th className={`${th} w-32 text-right max-md:hidden`}>Findings</th>}
          {repos && <th className={`${th} w-28 text-right max-md:hidden`}>Median time</th>}
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.key} className="border-b border-line">
            <td className="truncate px-2 py-2.5">
              <RowName row={row} />
            </td>
            <td className={`${num} text-fg tabular-nums`}>{compactTokens(row.tokens)}</td>
            <td className="px-2 py-2.5 max-md:hidden">
              <ShareBar tokens={row.tokens} total={total} />
            </td>
            {!repos && <td className={`${num} max-md:hidden`}>{compactTokens(row.cacheReadTokens)}</td>}
            <td className={`${num} max-md:hidden`}>{row.runs.toLocaleString()}</td>
            {repos && (
              <td className={`${num} text-caption max-md:hidden`}>
                <FindingsCell found={row.findingsFound ?? 0} posted={row.findings ?? 0} />
              </td>
            )}
            {repos && <td className={`${num} max-md:hidden`}>{formatFlowDuration(row.medianDurationMs)}</td>}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function UsageBreakdown({ report, query }: { report: UsageReport; query: UsageQuery }) {
  const [tab, setTab] = useState<TabId>("prs");
  const showAccounts = attributedRows(report.byAccount).length >= 2;
  const tabs = [
    { id: "prs", label: "Merge requests" },
    { id: "repos", label: "Repositories" },
    { id: "models", label: "Models" },
    ...(showAccounts ? [{ id: "accounts", label: "Accounts" }] : []),
  ];
  const active: TabId = tabs.some((item) => item.id === tab) ? tab : "prs";
  const total = report.totals.tokens;
  return (
    <section className="flex flex-col gap-3" aria-label="Usage breakdown">
      <ArrowTabs
        tabs={tabs}
        activeId={active}
        panelId={PANEL_ID}
        label="Break down usage by"
        previousLabel="Previous breakdown"
        nextLabel="Next breakdown"
        showArrows={false}
        onChange={(id) => setTab(id as TabId)}
      />
      <div id={PANEL_ID} role="tabpanel" aria-labelledby={`${PANEL_ID}-tab-${active}`}>
        {active === "prs" ? (
          <PrUsageList query={query} totalTokens={total} live={report.hasRunning} />
        ) : active === "repos" ? (
          <GroupTable rows={report.byRepo} total={total} kind="repos" />
        ) : active === "models" ? (
          <GroupTable rows={report.byModel} total={total} kind="models" />
        ) : (
          <div className="flex flex-col gap-2">
            <GroupTable rows={report.byAccount} total={total} kind="accounts" />
            {report.byAccount.some((row) => row.key === "unattributed") && (
              <p className="text-caption text-fg-subtle">
                Unattributed covers runs from before usage was recorded per account.
              </p>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
