import { useQuery } from "@tanstack/react-query";
import { cx } from "../../components/DesignSystem";
import type { AiAccount } from "../../types";
import { accountHealth, percentLeft, resetText, usageQuery, windowName } from "./AccountsPanel";

const shortName = (minutes: number | null) =>
  minutes === 300 ? "5h" : minutes === 10_080 ? "7d" : windowName(minutes);

/** How much of the selected account's plan is left, one slim bar per window. */
export function AccountUsageBar({ account }: { account: AiAccount }) {
  const usage = useQuery(usageQuery).data?.find((item) => item.accountId === account.id);
  const resting = accountHealth(account) === "resting";
  if (!usage?.windows.length) return null;
  return (
    <div className="flex items-center gap-3 text-caption text-fg-subtle max-md:hidden">
      {usage.windows.map((window) => {
        const left = resting ? 0 : percentLeft(window);
        const label = `${windowName(window.windowMins)} ${left}% restante`;
        return (
          <div
            key={window.windowMins ?? 0}
            className="flex items-center gap-1.5"
            title={[label, resetText(window.resetsAt)].filter(Boolean).join(" · ")}
          >
            <span>{shortName(window.windowMins)}</span>
            {/* biome-ignore lint/a11y/useSemanticElements: slim custom bar; a native <meter> renders with browser-specific styling */}
            <span
              className="h-1.5 w-14 overflow-hidden rounded-full bg-surface-sunken"
              role="meter"
              aria-label={`${windowName(window.windowMins)} de uso restante`}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={left}
            >
              <span
                className={cx(
                  "block h-full rounded-full",
                  left <= 10 ? "bg-danger" : left <= 30 ? "bg-warning" : "bg-success",
                )}
                style={{ width: `${left}%` }}
              />
            </span>
            <span className="tabular-nums text-fg-muted">{left}%</span>
          </div>
        );
      })}
    </div>
  );
}
