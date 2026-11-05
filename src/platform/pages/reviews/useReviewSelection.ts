import { parseAsInteger, useQueryState } from "nuqs";
import { useEffect, useState } from "react";
import type { ReviewCard } from "../../../types";

/**
 * The merge request the Reviews workspace monitors. A `?pr=` link (from Usage) picks it once the board
 * lists it; otherwise the first PR under review, then the first PR, is picked whenever the selection is gone.
 */
export function useReviewSelection(items: ReviewCard[]) {
  const [linkedId, setLinkedId] = useQueryState("pr", parseAsInteger.withOptions({ history: "replace" }));
  const [monitoringId, setMonitoringId] = useState<number | null>(null);
  const monitoring = items.find((item) => item.id === monitoringId) ?? null;
  useEffect(() => {
    const linked = linkedId === null ? undefined : items.find((item) => item.id === linkedId);
    if (linked) setMonitoringId(linked.id);
    else if (!monitoring)
      setMonitoringId(items.find((item) => ["reviewing", "queued"].includes(item.lane))?.id ?? items[0]?.id ?? null);
    // The link is consumed once the board has loaded, so picking another PR is not undone by it.
    if (linkedId !== null && items.length) void setLinkedId(null);
  }, [items, monitoring, linkedId, setLinkedId]);
  return { monitoringId, setMonitoringId, monitoring };
}
