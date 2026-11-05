import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { NodeChange } from "@xyflow/react";
import type { ComponentProps, ComponentType, ReactNode } from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { ReviewContextPlan, ReviewRun, ReviewRunStage } from "../../types";
import type { ReviewFlowNode } from "../lib/reviewFlow";
import { ReviewFlow } from "./ReviewFlow";
import { ContextPhaseDetails } from "./ReviewFlowNodes";

const flowCapture = vi.hoisted(() => ({
  nodes: [] as ReviewFlowNode[],
  onNodesChange: (_changes: NodeChange<ReviewFlowNode>[]) => {},
}));

vi.mock("../../services/setupApi", () => ({ setupApi: {} }));
vi.mock("@xyflow/react", () => ({
  Background: () => null,
  BackgroundVariant: { Dots: "dots" },
  Controls: () => null,
  Handle: ({ type }: { type: string }) => <span data-handle={type} />,
  Position: { Left: "left", Right: "right" },
  useUpdateNodeInternals: () => () => undefined,
  ReactFlow: ({
    nodes,
    nodeTypes,
    children,
    autoPanOnNodeFocus,
    onNodesChange,
  }: {
    nodes: ReviewFlowNode[];
    nodeTypes: Record<string, ComponentType<{ id: string; data: ReviewFlowNode["data"]; selected?: boolean }>>;
    children: ReactNode;
    autoPanOnNodeFocus: boolean;
    onNodesChange: typeof flowCapture.onNodesChange;
  }) => {
    Object.assign(flowCapture, { nodes, onNodesChange });
    return (
      <div data-auto-pan={autoPanOnNodeFocus}>
        {nodes.map((node) => {
          const Component = nodeTypes[node.type!];
          return <Component key={node.id} id={node.id} data={node.data} selected={node.selected} />;
        })}
        {children}
      </div>
    );
  },
}));

function renderView(selectedView: string | null, props: Partial<ComponentProps<typeof ReviewFlow>> = {}) {
  const client = new QueryClient();
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <ReviewFlow
        runStatus="posted"
        selectedView={selectedView}
        onSelectView={() => undefined}
        reviewDetails={
          <>
            <p>Final findings</p>
            <p>Candidate audit</p>
          </>
        }
        {...props}
      />
    </QueryClientProvider>,
  );
  client.clear();
  return html;
}

describe("review scope selection", () => {
  const context: ReviewContextPlan = {
    repoId: 813,
    changedFiles: ["docs/business-rules/programs.md"],
    packages: [],
    dependentPackages: [],
    relatedFiles: [],
    testFiles: [],
    conventionFiles: [],
    omittedCount: 0,
    incremental: false,
    range: "base...head",
    headSha: "head",
    baseSha: "head",
    priorSha: "previous",
  };

  it("shows one base-sync file and nineteen reused files without labeling the run full", async () => {
    const reused = Array.from({ length: 19 }, (_, index) => `source-${index}.ts`);
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const container = document.createElement("div");
    const root = createRoot(container);
    await act(async () =>
      root.render(
        <ContextPhaseDetails
          context={{
            ...context,
            scopeSelection: {
              mode: "base_sync",
              requestedFull: false,
              sourceRunId: 1487,
              fullPrFiles: [...context.changedFiles, ...reused],
              reusedFiles: reused,
            },
          }}
        />,
      ),
    );
    expect(container.textContent).toContain("1 file to review · 19 with reused coverage · 20 in the PR");
    expect(container.textContent).toContain("Base synchronization");
    const reusedGroup = [...container.querySelectorAll<HTMLButtonElement>(".review-context-file-group")].find(
      (group) => group.textContent === "19Reused",
    )!;
    await act(async () => reusedGroup.click());
    expect(reusedGroup.getAttribute("aria-pressed")).toBe("true");
    expect(container.textContent).toContain("Verified coverage from run 1487");
    expect(container.textContent).not.toContain("Full review:");
    expect(container.querySelector(".review-context-range")?.textContent).toBe(
      "git diff 'base...head' -- 'docs/business-rules/programs.md'",
    );
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  });

  it("quotes scoped diff arguments without allowing shell expansion", () => {
    const paths = [
      "src/$(printf injected).ts",
      "src/`printf injected`.ts",
      "src/$HOME.ts",
      "src/a'b.ts",
      "src/space name.ts",
      "src/back\\slash.ts",
    ];
    const html = renderToStaticMarkup(
      <ContextPhaseDetails
        context={{
          ...context,
          changedFiles: paths,
          scopeSelection: {
            mode: "content_reuse",
            requestedFull: false,
            sourceRunId: 1278,
            fullPrFiles: paths,
            reusedFiles: [],
          },
        }}
      />,
    );
    const container = document.createElement("div");
    container.innerHTML = html;
    const command = container.querySelector(".review-context-range")!.textContent!;
    const args = execFileSync("sh", ["-c", `printf '%s\\0' ${command.slice("git diff ".length)}`], { encoding: "utf8" })
      .split("\0")
      .slice(0, -1);
    expect(args).toEqual([context.range, "--", ...paths]);
  });

  it("does not display an unscoped full diff command when all files are reused", () => {
    const html = renderToStaticMarkup(
      <ContextPhaseDetails
        context={{
          ...context,
          changedFiles: [],
          scopeSelection: {
            mode: "base_sync",
            requestedFull: false,
            sourceRunId: 1487,
            fullPrFiles: context.changedFiles,
            reusedFiles: context.changedFiles,
          },
        }}
      />,
    );
    const container = document.createElement("div");
    container.innerHTML = html;
    expect(container.textContent).toContain("0 files to review · 1 with reused coverage");
    expect(container.querySelector(".review-context-range")).toBeNull();
  });

  it("labels explicit reprocessing without implying full scope or cached analysis", () => {
    const html = renderToStaticMarkup(
      <ContextPhaseDetails
        context={{
          ...context,
          scopeSelection: {
            mode: "reprocess",
            requestedFull: false,
            sourceRunId: 1522,
            fullPrFiles: [...context.changedFiles, "other.ts"],
            reusedFiles: ["other.ts"],
          },
        }}
      />,
    );
    const container = document.createElement("div");
    container.innerHTML = html;
    expect(container.textContent).toContain("Explicit re-run: fresh analysis");
    expect(container.textContent).toContain("without cached model results");
    expect(container.textContent).not.toContain("Full review:");
    expect(container.querySelector(".review-context-range")?.textContent).toBe(
      "git diff 'base...head' -- 'docs/business-rules/programs.md'",
    );
  });

  it("distinguishes content reuse and policy exclusions from full or fresh analysis", () => {
    const html = renderToStaticMarkup(
      <ContextPhaseDetails
        context={{
          ...context,
          changedFiles: [],
          scopeSelection: {
            mode: "content_reuse",
            requestedFull: false,
            sourceRunId: 1278,
            fullPrFiles: ["source.ts", "pnpm-lock.yaml"],
            reusedFiles: ["source.ts"],
            excludedFiles: [{ path: "pnpm-lock.yaml", reason: "generated" }],
          },
        }}
      />,
    );
    const container = document.createElement("div");
    container.innerHTML = html;
    expect(container.textContent).toContain("Content-verified reuse");
    expect(container.textContent).toContain("1 excluded, not reviewed");
    const excludedGroup = [...container.querySelectorAll(".review-context-file-group")].find((group) =>
      group.textContent?.endsWith("Excluded"),
    );
    expect(excludedGroup?.textContent).toBe("1Excluded");
    expect(container.textContent).not.toContain("Full review:");
    expect(container.querySelector(".review-context-range")).toBeNull();
  });

  it("preserves historical labels without inventing coverage metrics", () => {
    const full = renderToStaticMarkup(<ContextPhaseDetails context={context} />);
    expect(full).toContain("Full review:");
    expect(full).not.toContain("with reused coverage");
    const incremental = renderToStaticMarkup(<ContextPhaseDetails context={{ ...context, incremental: true }} />);
    expect(incremental).toContain("Incremental review:");
    expect(incremental).not.toContain("Base synchronization:");
  });
});

describe("review inspector navigation", () => {
  it.each(["context", "merge"])("keeps the latest completed %s stage green while the next stage runs", (id) => {
    const style = document.createElement("style");
    style.textContent = [
      ...readFileSync("src/styles/components.css", "utf8").matchAll(
        /\.review-flow-node-(?:completed|last-completed)(?: \.review-flow-node-icon)?\s*\{[^}]*\}/g,
      ),
    ]
      .map((match) => match[0])
      .join("\n");
    const container = document.createElement("div");
    container.innerHTML = renderView(null, {
      runStatus: "running",
      phases: [
        { id, label: id, status: "ok" },
        { id: "final", label: "Final", status: "running" },
      ],
    });
    document.head.append(style);
    document.body.append(container);
    try {
      const node = container.querySelector<HTMLElement>(`[data-phase-id="${id}"]`)!;
      expect(node.classList.contains("review-flow-node-completed")).toBe(true);
      expect(node.classList.contains("review-flow-node-last-completed")).toBe(true);
      expect(getComputedStyle(node).getPropertyValue("--review-flow-node-border")).toContain("var(--success)");
      expect(getComputedStyle(node.querySelector(".review-flow-node-icon")!).color).toBe("var(--success)");
      expect(container.querySelector('[data-phase-id="final"]')?.classList.contains("review-flow-node-running")).toBe(
        true,
      );
    } finally {
      container.remove();
      style.remove();
    }
  });

  it("pushes a disclosure's count to the right and mutes it", () => {
    const style = document.createElement("style");
    style.textContent = [...readFileSync("src/styles/components.css", "utf8").matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .filter((match) => match[1].includes(".disclosure > summary > span"))
      .map((match) => match[0])
      .join("\n");
    const container = document.createElement("div");
    container.innerHTML = '<details class="disclosure"><summary>Model runs<span>1</span></summary></details>';
    document.head.append(style);
    document.body.append(container);
    try {
      const count = getComputedStyle(container.querySelector("summary > span")!);
      expect(count.marginLeft).toBe("auto");
      expect(count.color).toBe("var(--text-3)");
    } finally {
      style.remove();
      container.remove();
    }
  });

  it("keeps a selected published result green instead of applying the purple selection highlight", () => {
    const style = document.createElement("style");
    style.textContent = [...readFileSync("src/styles/components.css", "utf8").matchAll(/([^{}]+)\{([^{}]*)\}/g)]
      .filter((match) =>
        match[1].split(",").every((selector) => /^\.review-flow-(?:node|result)/.test(selector.trim())),
      )
      .map((match) => match[0])
      .join("\n");
    const container = document.createElement("div");
    container.innerHTML = renderView("ready", {
      phases: [{ id: "ready", label: "Pronto", status: "ok" }],
      result: { status: "Published", detail: "2 findings", state: "completed" },
    });
    document.head.append(style);
    document.body.append(container);
    try {
      const result = container.querySelector<HTMLElement>('[data-phase-id="ready"]')!;
      expect(result.textContent).toContain("Published");
      expect(result.classList.contains("review-flow-node-completed")).toBe(true);
      expect(result.classList.contains("review-flow-node-selected")).toBe(true);
      expect(getComputedStyle(result).getPropertyValue("--review-flow-node-border")).toContain("var(--success)");
      expect(getComputedStyle(result).boxShadow).toContain("var(--success)");
      expect(getComputedStyle(result.querySelector(".review-flow-node-icon")!).color).toBe("var(--success)");
    } finally {
      container.remove();
      style.remove();
    }
  });

  it("distinguishes chunk preparation failure from a failed model execution", () => {
    const phase = {
      id: "chunks",
      label: "Chunks",
      status: "failed",
      inferenceSkipped: true,
      detail: "Chunk preparation failed before model execution.",
    };
    const run = {
      id: 10,
      prId: 1,
      status: "failed",
      inputTokens: 0,
      errorMessage: "Context metadata exceeds the input budget",
      tail: phase.detail,
    } as ReviewRun;
    const html = renderView("chunks", { run, runStatus: "failed", phases: [phase] });
    expect(html).toContain("Preparação falhou");
    expect(html).toContain("Nenhuma execução de modelo iniciada");
    expect(html).toContain("0 tokens");
    expect(html).not.toContain("Detalhes do chunk indisponíveis");
  });

  it("offers Re-run full in the failed stage inspector when coverage reuse is unsafe", () => {
    const onFullReview = vi.fn();
    const html = renderView("context", {
      onFullReview,
      runStatus: "failed",
      phases: [{ id: "context", label: "Contexto", status: "failed" }],
      run: {
        id: 10,
        prId: 1,
        status: "failed",
        errorMessage:
          "Cannot safely reuse the previous review: Persisted review coverage does not match its Git snapshot. Choose Re-run full to review the entire PR. No inference started.",
      } as ReviewRun,
    });
    expect(html).toContain(">Re-run full</button>");
  });
  it("uses the skip icon for every skipped stage and keeps pending and failed icons distinct", () => {
    const icon = (selectedView: string, props: Partial<ComponentProps<typeof ReviewFlow>>) => {
      const container = document.createElement("div");
      container.innerHTML = renderView(selectedView, props);
      const pill = container.querySelector(".review-flow-status-pill")!;
      return { text: pill.textContent, icon: pill.querySelector("svg")?.getAttribute("class") ?? "" };
    };
    const review = icon("review", {
      runStatus: "skipped",
      run: { id: 10, prId: 1, status: "skipped" } as ReviewRun,
      phases: [
        { id: "review", label: "Revisão", status: "skipped", inferenceSkipped: true },
        { id: "ready", label: "Pronto", status: "ok" },
      ],
    });
    expect(review).toMatchObject({ text: "Ignorado" });
    expect(review.icon).toContain("lucide-skip-forward");
    const pending = icon("final", {
      runStatus: "reviewing",
      phases: [
        { id: "chunks", label: "Chunks", status: "running" },
        { id: "final", label: "Final", status: "pending" },
      ],
    });
    expect(pending.text).toBe("Pendente");
    expect(pending.icon).not.toContain("lucide-skip-forward");
    const failed = icon("chunks", {
      runStatus: "failed",
      run: { id: 10, prId: 1, status: "failed" } as ReviewRun,
      phases: [{ id: "chunks", label: "Chunks", status: "failed", inferenceSkipped: true }],
    });
    expect(failed.text).toBe("Preparação falhou");
    expect(failed.icon).not.toContain("lucide-skip-forward");
  });

  it("shows cached chunks with known zero tokens instead of unknown usage", () => {
    const run = { id: 10, prId: 1, status: "preview", tail: "Done" } as ReviewRun;
    const stages = [
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
        fileCount: 2,
      },
    ] as ReviewRunStage[];
    const html = renderView("chunk:1", {
      run,
      stages,
      runStatus: "preview",
      phases: [{ id: "chunks", label: "Chunks", status: "ok" }],
    });
    expect(html).toContain("Em cache");
    expect(html).toContain("0 tokens");
    expect(html).toContain("Validated cache hit");
    expect(html).not.toContain("Tokens unavailable");
  });

  it("uses semantic icons before and after running, and the mascot eye during execution", () => {
    const run = {
      id: 10,
      prId: 1,
      status: "running",
      tail: "chunks progress: 0/2 done, 1 running\nchunk status:\n1 running\n2 queued",
    } as ReviewRun;
    const html = renderView(null, {
      run,
      runStatus: "running",
      phases: [
        { id: "index", label: "Preparar repositório", status: "ok" },
        { id: "context", label: "Contexto", status: "running" },
        { id: "chunks", label: "Chunks", status: "running" },
      ],
    });
    expect(html).toContain('data-review-stage-icon="index"');
    expect(html).toContain('data-review-stage-icon="context"');
    expect(html).toContain('data-review-stage-icon="chunks"');
    expect(html).toContain('data-review-stage-icon="chunk"');
    expect(html).toContain('data-review-chunk-number="1"');
    expect(html).toContain('data-review-chunk-number="2"');
    const document = new DOMParser().parseFromString(html, "text/html");
    expect(document.querySelector('[data-review-stage-icon="context"] .brand-loading-icon')).not.toBeNull();
    expect(document.querySelector(".lucide-loader-circle, .review-flow-spinning")).toBeNull();
    expect(html).toContain("review-flow-node-last-completed");

    const simpleReview = renderView(null, { phases: [{ id: "review", label: "Revisão", status: "pending" }] });
    expect(simpleReview).toContain('data-review-stage-icon="review"');
  });

  it("shows the immediate base and immutable scope in context details", () => {
    const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
    const run = { id: 10, prId: 1, status: "preview" } as ReviewRun;
    client.setQueryData(["reviews", 1, "runContext", 10, "ok"], {
      baseRef: "feature/parent",
      reviewedBaseSha: "parent0123456789",
      mergeBaseSha: "merge0123456789",
      scopeReason: "Full review after restack",
      incremental: false,
      range: "parent...child",
      headSha: "child0123456789",
      baseSha: "child0123456789",
      changedFiles: [],
      relatedFiles: [],
      testFiles: [],
      conventionFiles: [],
      packages: [],
      dependentPackages: [],
      omittedCount: 0,
    });
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <ReviewFlow
          run={run}
          runStatus="preview"
          phases={[{ id: "context", label: "Contexto", status: "ok" }]}
          selectedView="context"
          onSelectView={() => undefined}
        />
      </QueryClientProvider>,
    );
    expect(html).toContain("Immediate base");
    expect(html).toContain("feature/parent");
    expect(html).toContain("parent012345");
    expect(html).toContain("Merge-base");
    expect(html).toContain("Full review after restack");
    client.clear();
  });
  it("executes once without opening an inspector, blocks busy clicks, and opens results by keyboard", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const container = document.createElement("div");
    const root = createRoot(container);
    const client = new QueryClient();
    const onClick = vi.fn();
    const onSelectView = vi.fn();
    const render = (busy: boolean) => (
      <QueryClientProvider client={client}>
        <ReviewFlow
          runStatus={null}
          selectedView={null}
          onSelectView={onSelectView}
          control={{ kind: "run", label: "Executar revisão", disabled: false, busy, onClick }}
        />
      </QueryClientProvider>
    );
    try {
      await act(async () => root.render(render(false)));
      await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Executar revisão"]')!.click());
      expect(onClick).toHaveBeenCalledTimes(1);
      expect(onSelectView).not.toHaveBeenCalled();
      const dimensions = { width: 216, height: 92 };
      await act(async () =>
        flowCapture.onNodesChange(flowCapture.nodes.map((node) => ({ id: node.id, type: "dimensions", dimensions }))),
      );
      await act(async () => root.render(render(true)));
      expect(flowCapture.nodes.every((node) => node.measured?.width === 216 && node.measured.height === 92)).toBe(true);
      await act(async () =>
        flowCapture.onNodesChange([{ id: "ready", type: "dimensions", dimensions: { width: 216, height: 100 } }]),
      );
      expect(flowCapture.nodes.find((node) => node.id === "index")?.measured).toEqual(dimensions);
      expect(flowCapture.nodes.find((node) => node.id === "ready")?.measured?.height).toBe(100);
      await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Executar revisão"]')!.click());
      expect(onClick).toHaveBeenCalledTimes(1);
      await act(async () =>
        container
          .querySelector('[data-phase-id="ready"]')!
          .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })),
      );
      expect(onSelectView).toHaveBeenCalledExactlyOnceWith("ready");
    } finally {
      await act(async () => root.unmount());
      client.clear();
      vi.unstubAllGlobals();
    }
  });
  it.each(["run", "stop", "rerun"] as const)("renders an accessible %s control before Index", (kind) => {
    const label = kind === "run" ? "Executar revisão" : kind === "stop" ? "Parar revisão" : "Reexecutar revisão";
    const props = { control: { kind, label, disabled: false, onClick: vi.fn() } };
    const html = renderView(null, props);
    expect(html).toContain(`aria-label="${label}"`);
    expect(html.indexOf(`aria-label="${label}"`)).toBeLessThan(html.indexOf('data-phase-id="index"'));
    expect(html).not.toContain("Review overview");
    expect(renderView(null, { control: { ...props.control, busy: true } })).toMatch(/disabled="" aria-busy="true"/);
    expect(renderView(null, { control: { ...props.control, disabled: true } })).toContain('disabled=""');
  });

  it("makes the result a terminal report with status and findings instead of stage metrics", () => {
    const html = renderView("ready", {
      phases: [{ id: "ready", label: "Pronto", status: "ok" }],
      result: { state: "completed", status: "Needs decision", detail: "5 findings" },
    });
    expect(html).toContain("Resultado da revisão");
    expect(html).toContain("Needs decision");
    expect(html).toContain("5 findings");
    expect(html).toContain('data-auto-pan="false"');
    expect(html).toContain('aria-pressed="true"');
    expect(html).not.toContain("tokens");
    expect(html).not.toContain('data-handle="source"');
  });

  it("shows a clean result as completed with a not-found icon", () => {
    const html = renderView("ready", {
      phases: [{ id: "ready", label: "Pronto", status: "ok" }],
      result: { state: "completed", status: "Concluído", detail: "Nada a sinalizar" },
    });
    expect(html).toContain("Concluído");
    expect(html).toContain("Nada a sinalizar");
    expect(html).toContain('data-review-result-icon="not-found"');
    expect(html).toContain("review-flow-node-completed");
    expect(html).not.toContain("review-flow-node-awaiting_action");
  });

  it("preserves a result entry for older phase snapshots and scopes publication retry to it", () => {
    const props = {
      phases: [{ id: "index", label: "Index", status: "failed" }],
      resultAction: <button type="button">Retry publication</button>,
    };
    expect(renderView(null, props)).toContain('data-phase-id="ready"');
    expect(renderView("ready", props)).toContain("Retry publication");
    expect(renderView("index", props)).not.toContain("Retry publication");
  });
  it.each(["overview", "ready"])("renders %s findings directly without the View dropdown", (view) => {
    const html = renderView(view);
    expect(html).toContain("Final findings");
    expect(html).toContain('aria-label="Fechar detalhes"');
    expect(html).not.toContain('aria-label="Detail view"');
    expect(html).not.toContain("<select");
  });
  it.each(["overview", "ready"])("keeps candidate inspection in the %s result panel", (view) => {
    const html = renderView(view);
    expect(html).toContain("Candidate audit");
    expect(html).toContain("Final findings");
  });
});
