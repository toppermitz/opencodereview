import { describe, expect, it } from "vitest";
import type { ReviewCard, ReviewPhase, ReviewRun, ReviewRunStage } from "../../types";
import {
  buildReviewChunks,
  buildReviewFlow,
  isReviewResultView,
  parseChunkProgress,
  parseReviewPhases,
  phaseLabel,
  phaseMetrics,
  reviewControl,
  reviewNeedsAttention,
  reviewResultSummary,
  reviewWorkspaceReducer,
  runMetrics,
  totalTokens,
} from "./reviewFlow";

const card = {
  lane: "manual_required",
  state: "open",
  headSha: "current",
  resultObservedHeadSha: "current",
} as ReviewCard;

describe("canvas actions and result", () => {
  it("connects the execution control before the first stage without changing stage layout", () => {
    const phases = parseReviewPhases(null);
    const control = { ...reviewControl(card, null), onClick: () => undefined };
    const flow = buildReviewFlow(phases, null, 3, undefined, control);
    expect(flow.nodes[0]).toMatchObject({
      id: "review-control",
      type: "reviewControl",
      position: { x: -96, y: 86 },
      selectable: false,
      style: { pointerEvents: "all" },
    });
    expect(flow.nodes.slice(1)).toEqual(buildReviewFlow(phases, null, 3).nodes);
    expect(flow.nodes.slice(1).every((node) => node.initialWidth === 216 && node.initialHeight === 92)).toBe(true);
    expect(flow.edges[0]).toMatchObject({ source: "review-control", target: "index" });
    expect(flow.nodes[0].data.control).toBe(control);
    expect(phaseLabel(phases.find((phase) => phase.id === "ready")!)).toBe("Resultado da revisão");
  });

  it("keeps stop available while queued or running, and disables ineligible starts", () => {
    expect(reviewControl(card, null)).toMatchObject({ kind: "run", label: "Executar revisão", disabled: false });
    expect(reviewControl({ ...card, lane: "queued" }, null).kind).toBe("stop");
    expect(reviewControl(card, { status: "running" } as ReviewRun).kind).toBe("stop");
    expect(reviewControl(card, { status: "preview" } as ReviewRun).kind).toBe("rerun");
    for (const lane of ["closed", "merged", "draft", "auto_skipped", "size_skipped"]) {
      expect(reviewControl({ ...card, lane } as ReviewCard, null).disabled).toBe(true);
    }
    expect(reviewControl({ ...card, resultPublicationStatus: "failed" }, null).kind).toBe("run");
  });

  it("distinguishes generation, decision and publication without inventing clean results", () => {
    expect(reviewResultSummary(card, null).status).toBe("Aguardando revisão");
    const run = { headSha: "current", status: "running" } as ReviewRun;
    expect(reviewResultSummary(card, run)).toMatchObject({ status: "Aguardando revisão", state: "pending" });
    expect(reviewResultSummary({ ...card, lane: "queued" }, { ...run, status: "posted" }).status).toBe("Na fila");
    expect(reviewResultSummary(card, { ...run, status: "failed" }).status).toBe("Revisão falhou");
    expect(reviewResultSummary({ ...card, lane: "ready" }, { ...run, status: "preview" }, 5)).toMatchObject({
      status: "Precisa da sua decisão",
      detail: "5 achados",
      state: "awaiting_action",
    });
    expect(reviewResultSummary({ ...card, lane: "ready" }, { ...run, status: "preview" }, 0)).toMatchObject({
      status: "Precisa da sua decisão",
      detail: "Nada a sinalizar",
      state: "awaiting_action",
    });
    expect(reviewResultSummary(card, { ...run, status: "preview" }).detail).toBe("Ver achados e detalhes");
    expect(
      reviewResultSummary(
        { ...card, resultState: "clean", resultPublicationStatus: "posted" },
        { ...run, status: "done" },
      ),
    ).toMatchObject({ status: "Concluído", detail: "Nada a sinalizar", state: "completed" });
    expect(reviewResultSummary({ ...card, resultPublicationStatus: "failed" }, null).status).toBe("Publicação falhou");
    expect(reviewResultSummary({ ...card, resultState: "clean", resultObservedHeadSha: "old" }, null).detail).not.toBe(
      "Nada a sinalizar",
    );
    expect(reviewResultSummary(card, { ...run, headSha: "old", status: "preview" }).status).toBe(
      "Resultado desatualizado",
    );
    expect(reviewResultSummary(card, { ...run, status: "posted" }).status).toBe("Publicado");
    expect(reviewResultSummary(card, { ...run, status: "skipped" }).status).toBe("Ignorado");
    expect(reviewResultSummary(card, { ...run, status: "approved" }).status).toBe("Marcado como revisado");
    expect(reviewResultSummary({ ...card, resultPublicationStatus: "superseded" }, null).status).toBe(
      "Resultado desatualizado",
    );
  });

  it("skipped reviews need no attention and remain manually rerunnable", () => {
    const skipped = { ...card, lane: "skipped", resultState: "skipped" } as ReviewCard;
    const run = { headSha: "current", status: "skipped" } as ReviewRun;
    expect(reviewNeedsAttention(skipped)).toBe(false);
    expect(reviewControl(skipped, run)).toMatchObject({ kind: "rerun", disabled: false });
    expect(reviewResultSummary(skipped, run)).toMatchObject({ status: "Ignorado", state: "completed" });
    for (const lane of ["manual_required", "ready", "failed", "needs_setup"])
      expect(reviewNeedsAttention({ lane, state: "open" })).toBe(true);
    expect(reviewNeedsAttention({ lane: "manual_required", state: "merged" })).toBe(false);
  });
});

describe("review workspace selection", () => {
  const initial = { view: null, processesOpen: true, restoreProcesses: true };

  it("uses one inspector and collapses processes for stages and overview", () => {
    const stage = reviewWorkspaceReducer(initial, { type: "inspect", view: "chunks" });
    expect(stage).toEqual({ view: "chunks", processesOpen: false, restoreProcesses: true });
    const overview = reviewWorkspaceReducer(stage, { type: "inspect", view: "overview" });
    expect(overview).toEqual({ view: "overview", processesOpen: false, restoreProcesses: true });
    expect(reviewWorkspaceReducer(overview, { type: "close" })).toEqual(initial);
  });

  it("renders the result directly in Ready without changing the selected stage", () => {
    expect(isReviewResultView("ready")).toBe(true);
    expect(isReviewResultView("overview")).toBe(true);
    expect([null, "context", "final", "findings", "chunk:1"].some(isReviewResultView)).toBe(false);
    const ready = reviewWorkspaceReducer(initial, { type: "inspect", view: "ready" });
    expect(ready.view).toBe("ready");
    expect(ready.processesOpen).toBe(false);
    expect(reviewWorkspaceReducer(ready, { type: "close" })).toEqual(initial);
  });

  it("preserves manually collapsed processes when closing details", () => {
    const hidden = reviewWorkspaceReducer(initial, { type: "toggle-processes" });
    const stage = reviewWorkspaceReducer(hidden, { type: "inspect", view: "context" });
    expect(reviewWorkspaceReducer(stage, { type: "close" }).processesOpen).toBe(false);
  });

  it("resets on a new run and keeps narrow-screen processes hidden", () => {
    const stage = reviewWorkspaceReducer(initial, { type: "inspect", view: "chunks" });
    const narrow = reviewWorkspaceReducer(stage, { type: "resize", wide: false });
    expect(narrow.view).toBe("chunks");
    expect(reviewWorkspaceReducer(narrow, { type: "close" }).processesOpen).toBe(false);
    expect(reviewWorkspaceReducer(stage, { type: "reset", wide: true })).toEqual(initial);
  });
});

describe("individual review chunks", () => {
  const phases = ["index", "context", "chunks", "merge", "final", "validate", "ready"].map((id) => ({
    id,
    label: id,
    status: id === "chunks" ? "running" : "pending",
  }));
  const tail =
    "progresso dos chunks: 2/4 concluídos, 1 em execução\nstatus dos chunks:\n1 concluído 10m51s\n2 em execução 11m11s\n3 concluído 4m38s\n4 na fila\nchunk atual: 2\n\nchunk 2/4: revisando 5 arquivo(s)\nActive chunk output";
  const run = { id: 10, status: "running", tail } as ReviewRun;

  it("uses structured worker states over log text and separates queue from execution time", () => {
    const progress = {
      id: 1,
      status: "queued" as const,
      queuedAt: 1000,
      startedAt: null,
      executionFinishedAt: null,
      finishedAt: null,
      fileCount: 5,
      cached: false,
      attempts: 0,
      log: "Na fila",
      error: null,
    };
    const phase = [{ id: "chunks", label: "Chunks", status: "running", chunks: [progress] }];
    const queued = buildReviewChunks(
      phase,
      { ...run, tail: "progresso dos chunks: 0/1 concluídos, 1 em execução\nstatus dos chunks:\n1 em execução" },
      [],
      null,
      4000,
    );
    expect(queued.counts).toMatchObject({ queued: 1, running: 0 });
    expect(queued.items[0].metrics.durationMs).toBeNull();
    expect(queued.items[0].queuedDurationMs).toBe(3000);
    const executing = [
      { ...phase[0], chunks: [{ ...progress, status: "running" as const, startedAt: 4000, attempts: 1 }] },
    ];
    const running = buildReviewChunks(executing, run, [], queued, 6000);
    expect(running.items[0].metrics.durationMs).toBe(2000);
    expect(running.items[0].queuedDurationMs).toBe(3000);
    const validating = [
      {
        ...phase[0],
        chunks: [
          { ...progress, status: "validating" as const, startedAt: 4000, executionFinishedAt: 6000, attempts: 1 },
        ],
      },
    ];
    const result = buildReviewChunks(validating, run, [], running, 9000);
    expect(result.items[0].validating).toBe(true);
    expect(result.items[0].metrics.durationMs).toBe(2000);
  });

  it("sums recovery attempts without hiding unavailable usage or counting another chunk", () => {
    const progress = {
      id: 1,
      status: "done" as const,
      queuedAt: 1000,
      startedAt: 2000,
      executionFinishedAt: 6000,
      finishedAt: 6100,
      fileCount: 1,
      cached: false,
      attempts: 2,
      log: "Recovered context",
      error: null,
    };
    const phase = [{ id: "chunks", label: "Chunks", status: "ok", chunks: [progress] }];
    const first = {
      id: 1,
      runId: 10,
      stage: "chunk",
      chunkId: 1,
      status: "retried",
      cacheHit: false,
      inputTokens: 250,
      createdAt: 2000,
      finishedAt: 3000,
    } as ReviewRunStage;
    const second = { ...first, id: 2, status: "done", inputTokens: 500, createdAt: 4000, finishedAt: 6000 };
    const result = buildReviewChunks(phase, { ...run, status: "preview" }, [first, second]);
    expect(result.items).toHaveLength(1);
    expect(result.items[0]).toMatchObject({
      state: "completed",
      attempts: 2,
      cached: false,
      metrics: { tokens: 750, partial: false },
    });
    const incompleteUsage = buildReviewChunks(phase, { ...run, status: "preview" }, [
      first,
      { ...second, inputTokens: 0 },
    ]);
    expect(incompleteUsage.items[0].metrics).toMatchObject({ tokens: 250, partial: true });
  });

  it("does not make cached zero usage partial or hide unknown execution usage", () => {
    const cached = {
      id: 1,
      runId: 10,
      stage: "chunk",
      chunkId: 1,
      status: "cached",
      cacheHit: true,
      inputTokens: 0,
      createdAt: 1000,
      finishedAt: 1001,
    } as ReviewRunStage;
    const phase = { id: "chunks", label: "Chunks", status: "ok" };
    expect(phaseMetrics(phase, "completed", [cached], run, 2000)).toMatchObject({ tokens: 0, partial: false });
    expect(phaseMetrics(phase, "completed", [{ ...cached, status: "done", cacheHit: false }], run, 2000)).toMatchObject(
      { tokens: null, partial: true },
    );
  });

  it("recognizes cached completion and known zero inference usage", () => {
    const rows = [
      {
        id: 1,
        runId: 10,
        stage: "chunk",
        chunkId: 1,
        status: "cached",
        cacheHit: true,
        inputTokens: 0,
        createdAt: 1000,
        finishedAt: 1001,
      },
    ] as ReviewRunStage[];
    const chunks = buildReviewChunks(phases, run, rows);
    expect(chunks.items[0].state).toBe("completed");
    expect(chunks.items[0].metrics.tokens).toBe(0);
    expect(chunks.items[0].metrics.partial).toBe(false);
  });

  it("shows every planned chunk and isolates the live log to its owner", () => {
    const chunks = buildReviewChunks(phases, run, []);
    expect(chunks.items.map((chunk) => [chunk.id, chunk.state])).toEqual([
      [1, "completed"],
      [2, "running"],
      [3, "completed"],
      [4, "queued"],
    ]);
    expect(chunks.counts).toMatchObject({ completed: 2, running: 1, queued: 1 });
    expect(chunks.total).toBe(4);
    expect(chunks.items[1].metrics.durationMs).toBe(671_000);
    expect(chunks.items[1].log).toContain("Active chunk output");
    expect(chunks.items[0].log).toBeNull();
    expect(chunks.items[1].metrics.tokens).toBeNull();
  });

  it("retains observed chunk output while other workers report, but not across runs", () => {
    const first = buildReviewChunks(phases, run, []);
    const nextTail = tail
      .replace("chunk atual: 2", "chunk atual: 4")
      .replace("chunk 2/4", "chunk 4/4")
      .replace("Active chunk output", "Fourth chunk output");
    const next = buildReviewChunks(phases, { ...run, tail: nextTail }, [], first);
    expect(next.items[1].log).toContain("Active chunk output");
    expect(next.items[3].log).toContain("Fourth chunk output");
    const fresh = buildReviewChunks(phases, { ...run, id: 11, tail: nextTail }, [], first);
    expect(fresh.items[1].log).toBeNull();
  });

  it("uses persisted outcomes and tokens without duplicating child nodes", () => {
    const stages = [
      {
        id: 1,
        runId: 10,
        stage: "chunk",
        chunkId: 2,
        status: "done",
        inputTokens: 500,
        createdAt: 1000,
        finishedAt: 7000,
        fileCount: 5,
      },
      {
        id: 2,
        runId: 10,
        stage: "chunk",
        chunkId: 2,
        status: "done",
        inputTokens: 250,
        createdAt: 8000,
        finishedAt: 9000,
        fileCount: 5,
      },
      { id: 3, runId: 99, stage: "chunk", chunkId: 1, status: "failed", inputTokens: 100000 },
    ] as ReviewRunStage[];
    const chunks = buildReviewChunks(phases, run, stages);
    expect(chunks.items).toHaveLength(4);
    expect(chunks.items[1].state).toBe("completed");
    expect(chunks.items[1].metrics.tokens).toBe(750);
    expect(chunks.items[1].metrics.durationMs).toBe(8000);
    expect(chunks.items[0].state).toBe("completed");
  });

  it("retains completed chunks after logs move to the next phase", () => {
    const finished = phases.map((phase) =>
      phase.id === "chunks"
        ? { ...phase, status: "ok", detail: "Completed 4 chunk reviews across 20 reviewable files." }
        : phase,
    );
    const chunks = buildReviewChunks(finished, { ...run, status: "preview", tail: "Review ready" }, []);
    expect(chunks.items).toHaveLength(4);
    expect(chunks.items.every((chunk) => chunk.state === "completed")).toBe(true);
    expect(chunks.items.every((chunk) => chunk.metrics.tokens === null)).toBe(true);
  });

  it("does not keep chunks running after cancellation or invent their failures", () => {
    expect(buildReviewChunks(phases, { ...run, status: "cancelled" }, []).items.map((chunk) => chunk.state)).toEqual([
      "completed",
      "cancelled",
      "completed",
      "cancelled",
    ]);
    expect(buildReviewChunks(phases, { ...run, status: "failed" }, []).items[1].state).toBe("unknown");
    const rows = [
      { id: 1, runId: 10, stage: "chunk", chunkId: 2, status: "failed", inputTokens: 300 },
    ] as ReviewRunStage[];
    expect(buildReviewChunks(phases, { ...run, status: "failed" }, rows).items[1].state).toBe("failed");
  });

  it("handles missing status rows and rejects unsafe or unrelated progress text", () => {
    expect(
      parseChunkProgress(
        "progresso dos chunks: 0/999999999 concluídos, 1 em execução\nstatus dos chunks:\n1 em execução",
      ),
    ).toBeNull();
    expect(parseChunkProgress("Agent quoted: progresso dos chunks: 1/2 concluídos, 1 em execução")).toBeNull();
    expect(parseChunkProgress("chunks progress: 1/2 done, 1 running\nchunk status:\n1 running")).not.toBeNull();
    const chunks = buildReviewChunks(
      phases,
      { ...run, tail: "progresso dos chunks: 0/2 concluídos, 2 em execução" },
      [],
    );
    expect(chunks.items.map((chunk) => chunk.state)).toEqual(["unknown", "unknown"]);
    expect(chunks.counts.unknown).toBe(2);
  });

  it("groups chunk children below their parent without overlapping later phases", () => {
    const chunks = buildReviewChunks(phases, run, []);
    const flow = buildReviewFlow(phases, "running", 3, chunks);
    const parent = flow.nodes.find((node) => node.id === "chunks")!;
    const children = flow.nodes.filter((node) => node.parentId === "chunks");
    const nextRow = flow.nodes.find((node) => node.id === "final")!;
    expect(children.map((node) => node.id)).toEqual(["chunk:1", "chunk:2", "chunk:3", "chunk:4"]);
    expect(nextRow.position.y).toBeGreaterThan(parent.position.y + Number(parent.style!.height));
    expect(
      children.every((node) => node.position.y >= 132 && node.position.y + 92 <= Number(parent.style!.height)),
    ).toBe(true);
    expect(flow.edges).toHaveLength(phases.length - 1);
    const completed = buildReviewFlow(
      phases,
      "running",
      3,
      buildReviewChunks(phases, { ...run, tail: tail.replace("2 running 11m11s", "2 done 11m12s") }, []),
    );
    expect(completed.nodes.map((node) => node.id)).toEqual(flow.nodes.map((node) => node.id));
  });
});

describe("review flow mapping", () => {
  it("keeps confirmed pre-inference planning failures at known zero usage", () => {
    const phases = [
      { id: "chunks", label: "Chunks", status: "failed", inferenceSkipped: true },
      { id: "final", label: "Final", status: "pending", inferenceSkipped: true },
    ];
    const run = { id: 1, status: "failed", inputTokens: 0, phaseJson: JSON.stringify(phases) } as ReviewRun;
    expect(runMetrics(run, [], 1000)).toMatchObject({ tokens: 0, partial: false });
    expect(phaseMetrics(phases[0], "failed", [], run, 1000)).toMatchObject({ tokens: 0, partial: false });
    expect(
      runMetrics(
        { ...run, phaseJson: JSON.stringify(phases.map(({ inferenceSkipped: _, ...phase }) => phase)) },
        [],
        1000,
      ).tokens,
    ).toBeNull();
  });
  it("shows skipped inference as known zero without inventing an execution", () => {
    const phases = [
      { id: "review", label: "Revisão", status: "skipped", inferenceSkipped: true },
      { id: "validate", label: "Validar", status: "skipped", inferenceSkipped: true },
      { id: "ready", label: "Pronto", status: "ok" },
    ];
    const run = { status: "preview", inputTokens: 0, phaseJson: JSON.stringify(phases) } as ReviewRun;
    expect(buildReviewFlow(phases, "preview").nodes.every((node) => node.data.state === "completed")).toBe(true);
    expect(phaseMetrics(phases[0], "completed", [], run, 1000)).toMatchObject({
      tokens: 0,
      partial: false,
      durationMs: null,
    });
    expect(runMetrics(run, [], 1000)).toMatchObject({ tokens: 0, partial: false });
  });
  it("keeps deep phases after a completed run", () => {
    const phases = parseReviewPhases(
      JSON.stringify([
        { id: "context", label: "Contexto pronto", status: "ok" },
        { id: "chunks", label: "Chunk reviews", status: "ok" },
        { id: "merge", label: "Merged findings", status: "ok" },
        { id: "final", label: "Final integration", status: "ok" },
        { id: "validate", label: "Validado", status: "ok" },
        { id: "ready", label: "Pronto", status: "ok" },
      ]),
    );
    const flow = buildReviewFlow(phases, "preview");
    expect(flow.nodes).toHaveLength(6);
    expect(flow.nodes.every((node) => node.data.state === "completed")).toBe(true);
    expect(flow.edges).toHaveLength(5);
  });

  it("marks the active phase and failure without changing topology", () => {
    const phases = parseReviewPhases(
      JSON.stringify([
        { id: "context", label: "Contexto pronto", status: "ok" },
        { id: "review", label: "Revisado", status: "running" },
        { id: "ready", label: "Pronto", status: "pending" },
      ]),
    );
    const running = buildReviewFlow(phases, "running");
    expect(running.nodes.map((node) => node.data.state)).toEqual(["completed", "running", "pending"]);
    const failed = buildReviewFlow(phases, "failed");
    expect(failed.nodes.map((node) => node.data.state)).toEqual(["completed", "failed", "pending"]);
  });

  it("colors the active and failed connection instead of its completed source", () => {
    const phases = parseReviewPhases(
      JSON.stringify([
        { id: "index", label: "Index", status: "ok" },
        { id: "context", label: "Contexto", status: "running" },
        { id: "review", label: "Revisão", status: "pending" },
      ]),
    );
    const running = buildReviewFlow(phases, "running");
    expect(running.edges[0].className).toBe("review-flow-edge-running");
    expect(running.edges[0].animated).toBe(true);
    expect(running.edges[1].animated).toBe(false);
    expect(running.edges[1].className).toBe("review-flow-edge-pending");
    const failed = buildReviewFlow(phases, "failed");
    expect(failed.edges[0].className).toBe("review-flow-edge-failed");
    expect(failed.edges.every((edge) => !edge.animated)).toBe(true);
  });

  it("marks the latest completed stage so it can stay highlighted beside the active stage", () => {
    const phases: ReviewPhase[] = [
      { id: "index", label: "Index", status: "ok" },
      { id: "context", label: "Contexto", status: "running" },
      { id: "review", label: "Revisão", status: "pending" },
    ];
    const flow = buildReviewFlow(phases, "running");
    expect(flow.nodes.find((node) => node.id === "index")?.data.lastCompleted).toBe(true);
    expect(flow.nodes.find((node) => node.id === "context")?.data.lastCompleted).toBe(false);
    expect(flow.nodes.find((node) => node.id === "review")?.data.lastCompleted).toBe(false);
  });

  it("does not turn pending stages green when output is ready", () => {
    const phases = parseReviewPhases(
      '[{"id":"index","label":"Index","status":"ok"},{"id":"review","label":"Revisão","status":"pending"}]',
    );
    expect(buildReviewFlow(phases, "preview").nodes[1].data.state).toBe("pending");
  });

  it("wraps deep runs without changing their ordered connections", () => {
    const phases = ["index", "context", "chunks", "merge", "final", "validate", "ready"].map((id) => ({
      id,
      label: id,
      status: "pending",
    }));
    const flow = buildReviewFlow(phases, null, 3);
    expect(flow.nodes.map((node) => node.position.x)).toEqual([0, 246, 492, 492, 246, 0, 0]);
    expect(flow.nodes[3].data.reverse).toBe(true);
    expect(flow.nodes[6].position.y).toBeGreaterThan(flow.nodes[3].position.y);
    expect(flow.edges.map((edge) => `${edge.source}-${edge.target}`)).toEqual([
      "index-context",
      "context-chunks",
      "chunks-merge",
      "merge-final",
      "final-validate",
      "validate-ready",
    ]);
  });

  it("shows local phases as zero tokens, but never guesses missing AI usage", () => {
    const local = phaseMetrics(
      { id: "context", label: "Contexto", status: "ok", startedAt: 1000, finishedAt: 9100 },
      "completed",
      [],
      null,
      50_000,
    );
    expect(local).toMatchObject({ durationMs: 8100, tokens: 0, partial: false });
    expect(
      phaseMetrics({ id: "chunks", label: "Chunks", status: "running", startedAt: 1000 }, "running", [], null, 5000),
    ).toMatchObject({ durationMs: 4000, tokens: null });
    expect(
      phaseMetrics({ id: "index", label: "Index", status: "pending" }, "pending", [], null, 5000).tokens,
    ).toBeNull();
  });

  it("sums real chunk tokens without including potential savings or final usage", () => {
    const rows = [
      { stage: "chunk", inputTokens: 1250, createdAt: 1000, finishedAt: 4000 },
      { stage: "chunk", inputTokens: 750, createdAt: 2000, finishedAt: 5000 },
      { stage: "final", inputTokens: 2000, createdAt: 5000, finishedAt: 7000 },
    ] as ReviewRunStage[];
    const phase = { id: "chunks", label: "Chunks", status: "running", startedAt: 1000 };
    expect(phaseMetrics(phase, "running", rows, null, 6000)).toMatchObject({
      durationMs: 5000,
      tokens: 2000,
      partial: true,
    });
    expect(phaseMetrics({ ...phase, finishedAt: 5000 }, "completed", rows, null, 10_000)).toMatchObject({
      durationMs: 4000,
      tokens: 2000,
      partial: false,
    });
    const run = { status: "preview", inputTokens: 4000, startedAt: 0, finishedAt: 7000 } as ReviewRun;
    expect(runMetrics(run, rows, 20_000)).toMatchObject({ durationMs: 7000, tokens: 4000, partial: false });
  });

  it("freezes failed stage time and keeps unreported tokens unknown", () => {
    const run = { status: "failed", finishedAt: 9000, inputTokens: 0 } as ReviewRun;
    const phase = { id: "review", label: "Revisão", status: "running", startedAt: 1000 };
    expect(phaseMetrics(phase, "failed", [], run, 50_000)).toMatchObject({ durationMs: 8000, tokens: null });
    const rows = [
      { stage: "single", inputTokens: 2000, createdAt: 1000, finishedAt: 9000, status: "failed" },
    ] as ReviewRunStage[];
    expect(phaseMetrics(phase, "failed", rows, run, 50_000)).toMatchObject({ tokens: 2000, partial: true });
  });

  it("falls back safely for malformed phase JSON", () => {
    const flow = buildReviewFlow(parseReviewPhases("not-json"), "queued");
    expect(flow.nodes.map((node) => node.id)).toEqual(["index", "context", "review", "validate", "ready"]);
    expect(flow.nodes[0].data.state).toBe("queued");
  });
});

it("counts processed tokens without the cache reads every model turn repeats", () => {
  expect(
    totalTokens({ inputTokens: 1_000, outputTokens: 500, cacheReadTokens: 1_200_000, cacheCreationTokens: 40_000 }),
  ).toBe(41_500);
});

describe("suggestions-only reviews", () => {
  it("say so on the result instead of reporting no findings", () => {
    const posted = {
      ...card,
      lane: "auto_posted",
      resultState: "findings_posted",
      suggestionsOnly: true,
    } as ReviewCard;
    const run = { status: "posted", headSha: "current" } as ReviewRun;
    expect(reviewResultSummary(posted, run, 0, 0).detail).toBe("Só sugestões");
    expect(reviewResultSummary({ ...posted, suggestionsOnly: false }, run, 0, 0).detail).toBe("Nada a sinalizar");
  });
});
