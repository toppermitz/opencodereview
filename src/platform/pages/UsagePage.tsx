import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { parseAsInteger, parseAsNumberLiteral, useQueryState } from "nuqs";
import { useState } from "react";
import { BrandLoading } from "../../components/BrandLoading";
import { DSButton, DSEmptyState, DSError, DSMenu } from "../../components/DesignSystem";
import { SegmentedControl } from "../../components/SegmentedControl";
import { setupApi } from "../../services/setupApi";
import type { UsageQuery, UsageRange, UsageReport } from "../../types";
import { PlatformPage } from "../components/PlatformHeader";
import { timezoneOffset, usageError } from "./usage/format";
import { ShareUsage } from "./usage/ShareUsage";
import { UsageBreakdown } from "./usage/UsageBreakdown";
import { UsageChart } from "./usage/UsageChart";
import { UsageHero } from "./usage/UsageHero";
import { UsageTotals } from "./usage/UsageTotals";

const RANGES = [7, 30, 90] as const satisfies UsageRange[];
const rangeOptions = RANGES.map((value) => ({ value, label: `${value}d` }));

function RepoFilter({
  report,
  repoId,
  onChange,
}: {
  report: UsageReport | undefined;
  repoId: number | null;
  onChange: (repoId: number | null) => void;
}) {
  const repos = report?.repos ?? [];
  if (repos.length < 2 && repoId === null) return null;
  const current =
    repoId === null ? "All repositories" : (repos.find((repo) => repo.id === repoId)?.fullName ?? "1 repository");
  return (
    <DSMenu
      label="Filtrar por repositório"
      text={current}
      chevron
      items={[
        { label: "All repositories", selected: repoId === null, onSelect: () => onChange(null) },
        ...repos.map((repo) => ({
          label: repo.fullName,
          selected: repo.id === repoId,
          onSelect: () => onChange(repo.id),
        })),
      ]}
    />
  );
}

function EmptyUsage({
  days,
  filtered,
  onShow90,
  onClear,
}: {
  days: UsageRange;
  filtered: boolean;
  onShow90: () => void;
  onClear: () => void;
}) {
  return (
    <DSEmptyState>
      <p>
        O uso aparece depois que uma revisão roda. Comece uma em{" "}
        <Link to="/platform/reviews" className="font-medium text-fg underline underline-offset-2">
          Revisões
        </Link>
        .
      </p>
      {(filtered || days < 90) && (
        <div className="mt-4 flex justify-center gap-2">
          {filtered && <DSButton onClick={onClear}>Limpar filtro</DSButton>}
          {days < 90 && <DSButton onClick={onShow90}>Mostrar 90 dias</DSButton>}
        </div>
      )}
    </DSEmptyState>
  );
}

export function UsagePage() {
  const [days, setDays] = useQueryState(
    "days",
    parseAsNumberLiteral(RANGES).withDefault(30).withOptions({ history: "replace" }),
  );
  const [repoId, setRepoId] = useQueryState("repo", parseAsInteger.withOptions({ history: "replace" }));
  const [tz] = useState(() => timezoneOffset());
  const query: UsageQuery = { days, repoId, tz };
  const report = useQuery({
    queryKey: ["usage", "report", days, repoId, tz],
    queryFn: () => setupApi.usageReport(query),
    placeholderData: keepPreviousData,
    refetchInterval: (current) => (current.state.data?.hasRunning ? 5000 : false),
  });
  const data = report.data;
  return (
    <PlatformPage
      title="Usage"
      description="Tokens used by reviews"
      action={
        <>
          <RepoFilter report={data} repoId={repoId} onChange={(id) => void setRepoId(id)} />
          <SegmentedControl
            label="Date range"
            options={rangeOptions}
            value={days}
            onChange={(value) => void setDays(value)}
          />
        </>
      }
    >
      <div className="flex w-full flex-col gap-10 px-6 pt-2 pb-8">
        {report.isError && !data ? (
          <DSError>{usageError(report.error)}</DSError>
        ) : !data ? (
          <BrandLoading label="Carregando uso…" />
        ) : data.totals.runs === 0 ? (
          <EmptyUsage
            days={days}
            filtered={repoId !== null}
            onShow90={() => void setDays(90)}
            onClear={() => void setRepoId(null)}
          />
        ) : (
          <>
            {report.isError && <DSError>{usageError(report.error)}</DSError>}
            <div className="grid gap-8 lg:grid-cols-[minmax(22rem,1fr)_minmax(0,2.6fr)] lg:items-stretch">
              <UsageHero report={data} />
              <UsageChart days={data.days} byDay={data.byDay} action={<ShareUsage report={data} />} />
            </div>
            <UsageTotals totals={data.totals} />
            <UsageBreakdown report={data} query={query} />
          </>
        )}
      </div>
    </PlatformPage>
  );
}
