import { AlertTriangle, ChevronRight } from "lucide-react";
import { useEffect, useState } from "react";
import { cx, toneDot, toneText } from "../../../components/DesignSystem";
import type { ReviewCard } from "../../../types";
import { Avatar } from "./Avatar";
import { byRecent, itemTime, labelCard, laneTone } from "./reviewHelpers";

export type ReviewSection = {
  id: string;
  title: string;
  lanes: string[];
  /** Open on a first visit; afterwards the browser remembers what the user opened. */
  defaultOpen: boolean;
  /** Below the divider: finished work the user looks back at, not work in flight. */
  history?: boolean;
  /** Its count is highlighted while it holds anything, because each item waits on the user. */
  attention?: boolean;
};

const OPEN_KEY = "ocr.reviewSectionsOpen";

function storedOpen(sections: ReviewSection[]): Record<string, boolean> {
  const defaults = Object.fromEntries(sections.map((section) => [section.id, section.defaultOpen]));
  try {
    const value: unknown = JSON.parse(localStorage.getItem(OPEN_KEY) ?? "{}");
    return value && typeof value === "object" ? { ...defaults, ...value } : defaults;
  } catch {
    return defaults;
  }
}

export function ReviewSections({
  compact = false,
  sections,
  items,
  expandAll,
  selectedId,
  attentionPrIds,
  now,
  onSelect,
}: {
  /** Avatars only, for a list dragged narrow. */
  compact?: boolean;
  sections: ReviewSection[];
  items: ReviewCard[];
  /** While searching or filtering, every section with a match is shown open. */
  expandAll: boolean;
  selectedId: number | null;
  attentionPrIds: Set<number>;
  now: number;
  onSelect: (card: ReviewCard) => void;
}) {
  const [open, setOpen] = useState(() => storedOpen(sections));
  const grouped = sections.map((section) => ({
    ...section,
    items: byRecent(items.filter((item) => section.lanes.includes(item.lane))),
  }));
  const selectedSectionId = grouped.find((section) => section.items.some((card) => card.id === selectedId))?.id;
  // A PR picked from a link or by default may sit in a collapsed section: open it once so the selection shows.
  useEffect(() => {
    if (selectedSectionId)
      setOpen((current) => (current[selectedSectionId] ? current : { ...current, [selectedSectionId]: true }));
  }, [selectedSectionId]);
  // While searching or filtering every matching section shows open; what the user collapses then is not saved,
  // and clearing the search brings back the sections exactly as they were.
  const [collapsedWhileFiltering, setCollapsedWhileFiltering] = useState<Set<string>>(new Set());
  useEffect(() => {
    if (!expandAll) setCollapsedWhileFiltering(new Set());
  }, [expandAll]);
  const toggle = (id: string) => {
    if (expandAll) {
      setCollapsedWhileFiltering((current) => {
        const next = new Set(current);
        if (!next.delete(id)) next.add(id);
        return next;
      });
      return;
    }
    setOpen((current) => {
      const next = { ...current, [id]: !current[id] };
      try {
        localStorage.setItem(OPEN_KEY, JSON.stringify(next));
      } catch {
        // Storage is a convenience; the section still toggles for this visit.
      }
      return next;
    });
  };
  if (compact)
    return (
      <div className="review-process-compact flex flex-col items-center gap-1.5 pt-1">
        {grouped
          .flatMap((section) => section.items)
          .map((card) => (
            <button
              key={card.id}
              type="button"
              aria-pressed={selectedId === card.id}
              aria-label={`${card.title} · ${labelCard(card)}`}
              title={`${card.title}\n${card.repoFullName.split("/").pop()} #${card.number} · ${labelCard(card)}`}
              className={cx(
                "relative grid size-12 cursor-pointer place-items-center rounded-xl transition-colors",
                selectedId === card.id ? "bg-surface-muted" : "hover:bg-surface-muted/50",
              )}
              onClick={() => onSelect(card)}
            >
              <Avatar card={card} />
              <span
                className={cx(
                  "absolute right-1.5 bottom-1.5 size-2 rounded-full ring-2 ring-surface",
                  toneDot[laneTone(card.lane)],
                )}
              />
            </button>
          ))}
      </div>
    );
  return (
    <div className="review-sections flex flex-col gap-1">
      {grouped.map((section, index) => {
        const empty = section.items.length === 0;
        const expanded = !empty && (expandAll ? !collapsedWhileFiltering.has(section.id) : Boolean(open[section.id]));
        const panelId = `review-section-${section.id}`;
        return (
          <section
            key={section.id}
            className={cx(
              "review-section flex flex-col",
              section.history &&
                !grouped[index - 1]?.history &&
                "review-section-history-start mt-2 border-t border-line pt-2",
            )}
            data-section={section.id}
          >
            <button
              type="button"
              className={cx(
                "review-section-toggle flex h-8 w-full items-center gap-2 rounded-lg px-2.5 text-caption font-semibold tracking-wide uppercase transition-colors",
                empty
                  ? "cursor-default text-fg-subtle opacity-50"
                  : "cursor-pointer text-fg-muted hover:bg-surface-muted/50 hover:text-fg",
                section.attention && !empty && "text-warning hover:text-warning",
              )}
              aria-expanded={expanded}
              aria-controls={panelId}
              disabled={empty}
              onClick={() => toggle(section.id)}
            >
              <ChevronRight
                className={cx("size-3.5 flex-none transition-transform", expanded && "rotate-90")}
                aria-hidden="true"
              />
              <span className="min-w-0 flex-1 truncate text-left">{section.title}</span>
              <span
                className={cx(
                  "review-section-count rounded-full px-1.5 font-medium tracking-normal tabular-nums",
                  section.attention && !empty && "bg-warning/15",
                )}
              >
                {section.items.length}
              </span>
            </button>
            {expanded && (
              <div id={panelId} className="review-section-items flex flex-col gap-1.5 pt-1 pb-2">
                {section.items.map((card) => (
                  <button
                    key={card.id}
                    type="button"
                    aria-pressed={selectedId === card.id}
                    className={`review-process-item flex w-full cursor-pointer items-center gap-3 rounded-xl border border-transparent px-2.5 py-2.5 text-left transition-colors ${selectedId === card.id ? "review-process-item-selected bg-surface-muted" : "hover:bg-surface-muted/50"}`}
                    onClick={() => onSelect(card)}
                  >
                    <Avatar card={card} />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline gap-2">
                        <div
                          className="review-process-item-pr-title min-w-0 flex-1 truncate text-small font-medium text-fg"
                          title={card.title}
                        >
                          {card.title}
                        </div>
                        <time
                          className="flex-none text-caption text-fg-subtle tabular-nums"
                          dateTime={
                            Number.isFinite(card.updatedAt) ? new Date(card.updatedAt).toISOString() : undefined
                          }
                        >
                          {itemTime(card.updatedAt, now)}
                        </time>
                      </div>
                      <div className="review-process-item-meta mt-0.5 flex items-center gap-2">
                        <span className="review-process-item-title min-w-0 flex-1 truncate text-caption text-fg-subtle">
                          {card.repoFullName.split("/").pop()} <span className="tabular-nums">#{card.number}</span>
                        </span>
                        {attentionPrIds.has(card.id) && (
                          <AlertTriangle className="size-3.5 flex-none text-warning" aria-label="Precisa de atenção" />
                        )}
                        <span
                          className={cx(
                            "review-process-lane inline-flex flex-none items-center gap-1.5 text-caption",
                            toneText[laneTone(card.lane)],
                          )}
                        >
                          <span className={cx("size-1.5 rounded-full", toneDot[laneTone(card.lane)])} />
                          {labelCard(card)}
                        </span>
                      </div>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
}
