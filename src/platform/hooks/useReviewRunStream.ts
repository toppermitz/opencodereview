import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { setupApi } from "../../services/setupApi";
import type { ReviewProgressSnapshot, ReviewRun } from "../../types";
import { required } from "../lib/utils";

const ACTIVE_STATUSES = new Set(["pending", "running"]);

type StreamState = "idle" | "connecting" | "live" | "reconnecting" | "fallback" | "closed";

export function useReviewRunStream(prId: number | null, restartKey = 0) {
  const queryClient = useQueryClient();
  const [streamState, setStreamState] = useState<StreamState>("idle");
  const [streamError, setStreamError] = useState<string | null>(null);
  const [lastUpdatedAt, setLastUpdatedAt] = useState<number | null>(null);
  const runQuery = useQuery({
    queryKey: ["reviews", prId, "run"],
    queryFn: () => setupApi.getReviewRun(required(prId, "merge request id")),
    enabled: prId !== null,
    refetchInterval: streamState === "fallback" ? 1500 : streamState === "closed" ? 5000 : false,
    refetchOnWindowFocus: false,
  });

  const recorded = runQuery.data !== undefined && (!runQuery.data || !ACTIVE_STATUSES.has(runQuery.data.status));
  // biome-ignore lint/correctness/useExhaustiveDependencies: prId, the run id and restartKey are triggers: clear the timestamp for a new PR, run or restart
  useEffect(() => setLastUpdatedAt(null), [prId, runQuery.data?.id, restartKey]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the run id and restartKey are triggers: reconnect the stream for a new run or a manual restart
  useEffect(() => {
    setStreamError(null);
    if (prId === null) {
      setStreamState("idle");
      return;
    }
    if (runQuery.isLoading || recorded) {
      setStreamState(recorded ? "closed" : "connecting");
      return;
    }
    const queryKey = ["reviews", prId, "run"] as const;
    const controller = new AbortController();
    let disposed = false;
    let retryTimer: number | null = null;
    let attempt = 0;
    let terminalSeen = false;

    const applySnapshot = (snapshot: ReviewProgressSnapshot | null) => {
      if (disposed || terminalSeen || controller.signal.aborted || (snapshot && snapshot.prId !== prId)) return;
      attempt = 0;
      setStreamError(null);
      terminalSeen = !snapshot || !ACTIVE_STATUSES.has(snapshot.status);
      setStreamState(terminalSeen ? "closed" : "live");
      if (!snapshot) {
        queryClient.setQueryData(queryKey, null);
        controller.abort();
        return;
      }
      setLastUpdatedAt(snapshot.updatedAt);
      const current = queryClient.getQueryData<ReviewRun | null>(queryKey);
      if (current?.id === snapshot.runId) {
        queryClient.setQueryData<ReviewRun | null>(queryKey, (previous) =>
          previous && previous.id === snapshot.runId
            ? {
                ...previous,
                status: snapshot.status,
                headSha: snapshot.headSha,
                errorMessage: snapshot.errorMessage,
                phaseJson: snapshot.phaseJson,
                tail: snapshot.tail,
                startedAt: snapshot.startedAt,
                finishedAt: snapshot.finishedAt,
              }
            : previous,
        );
      }
      if (current?.id !== snapshot.runId || terminalSeen) void queryClient.refetchQueries({ queryKey });
      if (terminalSeen) controller.abort();
    };

    const scheduleRetry = () => {
      if (disposed || terminalSeen) return;
      attempt = Math.min(attempt + 1, 6);
      const delay = Math.min(1000 * 2 ** attempt, 10000) * (0.8 + Math.random() * 0.2);
      setStreamState("fallback");
      retryTimer = window.setTimeout(connect, delay);
    };

    const connect = () => {
      if (disposed || terminalSeen) return;
      setStreamState(attempt === 0 ? "connecting" : "fallback");
      setupApi
        .streamReviewProgress(prId, controller.signal, applySnapshot)
        .then(() => {
          if (disposed) return;
          if (terminalSeen) {
            setStreamState("closed");
            return;
          }
          scheduleRetry();
        })
        .catch((cause: unknown) => {
          if (disposed || controller.signal.aborted) return;
          setStreamError(cause instanceof Error ? cause.message : String(cause));
          scheduleRetry();
        });
    };

    connect();
    return () => {
      disposed = true;
      controller.abort();
      if (retryTimer !== null) window.clearTimeout(retryTimer);
    };
  }, [prId, queryClient, runQuery.isLoading, runQuery.data?.id, recorded, restartKey]);

  return { ...runQuery, streamState, streamError, lastUpdatedAt };
}
