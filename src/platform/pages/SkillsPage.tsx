import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { Check, Copy, FileText, MoreHorizontal, RotateCcw, Save, Trash2 } from "lucide-react";
import { lazy, Suspense, useMemo, useState } from "react";
import { BrandLoading } from "../../components/BrandLoading";
import { DSButton, DSEmptyState, DSError } from "../../components/DesignSystem";
import { setupApi } from "../../services/setupApi";
import type { SkillVersion } from "../../types";
import { PlatformPage } from "../components/PlatformHeader";
import { formatError, required } from "../lib/utils";

const SkillEditor = lazy(() => import("../components/SkillEditor").then((module) => ({ default: module.SkillEditor })));

export function SkillsPage() {
  const queryClient = useQueryClient();
  // `selectedId === null` = nothing explicitly selected → preview the active
  // skill. Using null (not 0) avoids colliding with the Default built-in's id 0,
  // which previously bounced any Default selection back to the active skill.
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [draft, setDraft] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [mode, setMode] = useState<"visual" | "markdown" | "diff">("visual");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState("");
  const versionsQuery = useQuery({ queryKey: ["skills", "versions"], queryFn: setupApi.listSkillVersions });
  const versions = versionsQuery.data ?? [];
  const active = useMemo(() => versions.find((version) => version.active) ?? versions[0], [versions]);
  const selected = (selectedId !== null ? versions.find((version) => version.id === selectedId) : undefined) ?? active;
  const contentQuery = useQuery({
    queryKey: ["skills", selected?.id ?? 0, "content"],
    queryFn: () => setupApi.getSkillVersionContent(required(selected, "skill version").id),
    enabled: Boolean(selected),
  });
  const baselineQuery = useQuery({
    queryKey: ["skills", 0, "content"],
    queryFn: () => setupApi.getSkillVersionContent(0),
    enabled: mode === "diff",
  });
  const content = draft ?? contentQuery.data ?? "";
  const dirty = draft !== null && draft !== contentQuery.data;
  const diffRows = useMemo(
    () => (mode === "diff" && baselineQuery.data !== undefined ? buildLineDiff(baselineQuery.data, content) : []),
    [mode, baselineQuery.data, content],
  );
  const activate = useMutation({
    mutationFn: setupApi.setActiveSkillVersion,
    onSuccess: (items) => {
      setError(null);
      queryClient.setQueryData(["skills", "versions"], items);
      setNotice("Version activated for future reviews.");
      void queryClient.invalidateQueries({ queryKey: ["skills"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const create = useMutation({
    mutationFn: setupApi.createSkillVersion,
    onSuccess: (version, savedContent) => {
      queryClient.setQueryData<SkillVersion[]>(["skills", "versions"], (items = []) => [
        ...items.map((item) => ({ ...item, active: false })),
        version,
      ]);
      queryClient.setQueryData(["skills", version.id, "content"], savedContent);
      setError(null);
      setDraft(null);
      setSelectedId(version.id);
      setMode("visual");
      setNotice(`${skillLabel(version)} saved and activated.`);
      void queryClient.invalidateQueries({ queryKey: ["skills"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const remove = useMutation({
    mutationFn: setupApi.deleteSkillVersion,
    onSuccess: (items) => {
      queryClient.setQueryData(["skills", "versions"], items);
      setError(null);
      setDraft(null);
      setSelectedId((items.find((item) => item.active) ?? items[0])?.id ?? null);
      setNotice("Version deleted.");
      void queryClient.invalidateQueries({ queryKey: ["skills"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const busy = activate.isPending || create.isPending || remove.isPending;
  const ready = Boolean(selected) && contentQuery.data !== undefined && !contentQuery.isError;
  useBlocker({
    shouldBlockFn: () => busy || (dirty && !window.confirm("Discard unsaved skill changes and leave this page?")),
    enableBeforeUnload: dirty || busy,
  });

  function selectVersion(id: number) {
    if (busy || id === selected?.id || (dirty && !window.confirm("Discard unsaved changes and open this version?")))
      return;
    setSelectedId(id);
    setDraft(null);
    setError(null);
    setNotice("");
    setRevision((value) => value + 1);
  }
  function updateDraft(value: string) {
    if (selected) setSelectedId(selected.id);
    setDraft(value);
    setNotice("");
  }
  async function copy() {
    try {
      await navigator.clipboard.writeText(content);
      setNotice("Markdown copied.");
    } catch (cause) {
      setError(`Could not copy Markdown: ${formatError(cause)}`);
    }
  }

  return (
    <PlatformPage
      title="Skills"
      description="Shape how OpenCodeReview reviews code. Edit a skill and save a new version when it’s ready."
    >
      <div className="px-6 pb-3 empty:hidden">{error && <DSError>{error}</DSError>}</div>
      <div className="review-workspace skills-workspace overflow-hidden max-md:flex-col [&_button:disabled]:cursor-not-allowed [&_button:disabled]:opacity-45 [&_select:disabled]:cursor-not-allowed [&_select:disabled]:opacity-45 flex min-h-0 flex-1 border-t border-line bg-canvas">
        <aside
          className="review-workspace-list skills-version-list flex w-80 shrink-0 flex-col border-r border-line bg-surface [&[hidden]]:hidden"
          aria-label="Skill versions"
        >
          <div className="review-workspace-list-header flex flex-none flex-col gap-3 border-b border-line p-4 [&_h2]:flex [&_h2]:items-center [&_h2]:gap-2 [&_h2]:text-overline [&_h2]:text-fg-subtle [&_h2]:uppercase">
            <h2>
              Versions <span>{versions.length}</span>
            </h2>
            <p>Saved snapshots stay unchanged.</p>
          </div>
          <div className="review-workspace-list-scroll flex min-h-0 flex-1 flex-col gap-1.5 overflow-y-auto p-3">
            {versionsQuery.isLoading ? (
              <BrandLoading size="compact" label="Loading skill versions…" />
            ) : versionsQuery.isError ? (
              <DSError>
                {formatError(versionsQuery.error)}{" "}
                <DSButton onClick={() => void versionsQuery.refetch()}>Retry</DSButton>
              </DSError>
            ) : (
              [
                { label: "Built-in", items: versions.filter((version) => version.id <= 0) },
                {
                  label: "Saved versions",
                  items: versions.filter((version) => version.id > 0).sort((a, b) => b.version - a.version),
                },
              ].map((group) => (
                <section
                  className="skill-version-group mb-5 flex flex-col gap-1.5 [&_h3]:mx-2 [&_h3]:my-1.5 [&_h3]:text-overline [&_h3]:text-fg-subtle [&_h3]:uppercase"
                  key={group.label}
                >
                  <h3>{group.label}</h3>
                  {group.items.map((version) => (
                    <SkillVersionRow
                      key={version.id}
                      version={version}
                      selected={selected?.id === version.id}
                      busy={busy}
                      onSelect={() => selectVersion(version.id)}
                    />
                  ))}
                  {group.items.length === 0 && (
                    <p className="skill-version-hint px-2 text-caption text-fg-muted">
                      Edit a skill and save your first version.
                    </p>
                  )}
                </section>
              ))
            )}
          </div>
          <div className="skill-sidebar-footer flex items-center gap-2 border-t border-line p-4 text-caption text-fg-muted max-md:hidden">
            <span className="skill-status-dot size-1.5 rounded-full bg-success" />
            {active ? `${skillLabel(active)} is active` : "No active version"}
          </div>
        </aside>
        <section
          className="review-workspace-main skill-editor-panel bg-canvas flex min-w-0 flex-1 flex-col overflow-hidden"
          aria-label="Skill editor"
        >
          <header className="review-workspace-head skill-editor-head max-md:flex-wrap flex flex-none items-center justify-between gap-4 border-b border-line bg-surface px-6 py-3.5 [&_h2]:text-heading [&_h2]:text-fg [&_p]:text-small [&_p]:text-fg-muted">
            <div className="min-w-0">
              <div className="skill-title-row flex flex-wrap items-center gap-2 text-small font-semibold text-fg">
                <FileText className="size-4 text-fg-muted" />
                <h2>{selected ? skillLabel(selected) : "Skill"}</h2>
                {selected?.active && (
                  <span className="skill-active-badge inline-flex h-5 items-center rounded-full bg-emerald-50 px-2 text-caption font-medium text-emerald-700 ring-1 ring-emerald-600/20 ring-inset">
                    Active
                  </span>
                )}
                {dirty && (
                  <span className="skill-draft-badge inline-flex h-5 items-center rounded-full bg-amber-50 px-2 text-caption font-medium text-amber-800 ring-1 ring-amber-600/25 ring-inset">
                    Unsaved changes
                  </span>
                )}
              </div>
              <p>
                {selected?.id !== undefined && selected.id <= 0 ? "Built-in review skill" : "Saved review skill"} ·
                SKILL.md
              </p>
            </div>
            <div className="review-workspace-actions flex items-center gap-2">
              <details
                className="skill-actions relative [&_summary]:grid [&_summary]:size-8 [&_summary]:cursor-pointer [&_summary]:list-none [&_summary]:place-items-center [&_summary]:rounded-md [&_summary]:border [&_summary]:border-line [&_summary]:bg-surface [&_summary]:text-fg-muted [&_summary]:shadow-card [&_summary:hover]:bg-surface-muted [&_summary::-webkit-details-marker]:hidden"
                key={selected?.id}
              >
                <summary aria-label="Version actions" title="Version actions">
                  <MoreHorizontal className="size-4" />
                </summary>
                <div className="skill-actions-menu absolute top-[calc(100%+6px)] right-0 z-20 flex min-w-48 flex-col gap-1 rounded-xl border border-line bg-surface p-1.5 shadow-pop [&_button]:justify-start">
                  <DSButton tone="ghost" disabled={!ready} onClick={() => void copy()}>
                    <Copy className="size-3.5" />
                    Copy Markdown
                  </DSButton>
                  {selected && !selected.active && (
                    <DSButton tone="ghost" disabled={busy || dirty} onClick={() => activate.mutate(selected.id)}>
                      <Check className="size-3.5" />
                      Use saved version
                    </DSButton>
                  )}
                  {selected && selected.id > 0 && (
                    <DSButton
                      tone="ghost"
                      disabled={busy}
                      onClick={() =>
                        window.confirm(
                          `Delete ${skillLabel(selected)}?${dirty ? " Unsaved changes will also be discarded." : ""}`,
                        ) && remove.mutate(selected.id)
                      }
                    >
                      <Trash2 className="size-3.5" />
                      Delete version
                    </DSButton>
                  )}
                </div>
              </details>
              <DSButton
                tone="primary"
                disabled={!ready || busy || !dirty || !content.trim()}
                onClick={() => create.mutate(content)}
              >
                <Save className="size-3.5" />
                {create.isPending ? "Salvando…" : "Save new version"}
              </DSButton>
            </div>
          </header>
          <div className="skill-view-bar flex flex-none items-center justify-between gap-3 border-b border-line bg-surface px-6 py-2.5 [&>span]:text-caption [&>span]:whitespace-nowrap [&>span]:text-fg-subtle max-md:px-3 max-md:[&>span]:hidden">
            <div
              className="skill-view-switch flex max-w-full min-w-0 gap-1 overflow-x-auto rounded-lg bg-surface-muted p-1 [&_[role=tab]]:flex-none [&_[role=tab]]:cursor-pointer [&_[role=tab]]:rounded-md [&_[role=tab]]:px-3 [&_[role=tab]]:py-1.5 [&_[role=tab]]:text-small [&_[role=tab]]:font-medium [&_[role=tab]]:whitespace-nowrap [&_[role=tab]]:text-fg-muted [&_[role=tab]:hover]:text-fg [&_[role=tab][aria-selected=true]]:bg-surface [&_[role=tab][aria-selected=true]]:text-brand [&_[role=tab][aria-selected=true]]:shadow-raised"
              role="tablist"
              aria-label="Editor view"
              onKeyDown={(event) => {
                if (busy || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
                const tabs = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]'));
                const index = tabs.indexOf(event.target as HTMLButtonElement);
                if (index < 0) return;
                event.preventDefault();
                const next =
                  event.key === "Home"
                    ? 0
                    : event.key === "End"
                      ? tabs.length - 1
                      : (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
                tabs[next].focus();
                tabs[next].click();
              }}
            >
              {(
                [
                  { id: "visual", label: "Visual" },
                  { id: "markdown", label: "Markdown" },
                  { id: "diff", label: "Diff vs default" },
                ] as const
              ).map((view) => (
                <button
                  key={view.id}
                  id={`skill-view-${view.id}`}
                  type="button"
                  role="tab"
                  aria-selected={mode === view.id}
                  aria-controls="skill-view-panel"
                  tabIndex={mode === view.id ? 0 : -1}
                  disabled={busy}
                  onClick={() => setMode(view.id)}
                >
                  {view.label}
                </button>
              ))}
            </div>
            <span>{content.length.toLocaleString()} chars</span>
          </div>
          <div
            id="skill-view-panel"
            className="skill-view-panel flex min-h-0 min-w-0 flex-1 flex-col focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-brand"
            role="tabpanel"
            aria-labelledby={`skill-view-${mode}`}
            // biome-ignore lint/a11y/noNoninteractiveTabindex: WAI-ARIA tabs pattern: the panel is focusable so keyboard users can reach its content after the tab list
            tabIndex={0}
          >
            {versionsQuery.isLoading ? null : !selected ? (
              <DSEmptyState>No skill versions available. Retry loading versions to begin.</DSEmptyState>
            ) : contentQuery.isError ? (
              <DSError>
                {formatError(contentQuery.error)} <DSButton onClick={() => void contentQuery.refetch()}>Retry</DSButton>
              </DSError>
            ) : !ready ? (
              <BrandLoading label="Loading skill content…" />
            ) : mode === "visual" ? (
              <Suspense fallback={<BrandLoading label="Loading editor…" />}>
                <SkillEditor
                  key={`${selected?.id}:${revision}`}
                  content={content}
                  disabled={busy}
                  onChange={updateDraft}
                />
              </Suspense>
            ) : mode === "markdown" ? (
              <textarea
                className="skill-markdown-source min-h-0 w-full flex-1 resize-none bg-canvas px-10 py-7 font-mono text-small leading-7 text-fg outline-none [tab-size:2] max-md:p-5"
                aria-label="Skill Markdown source"
                spellCheck={false}
                value={content}
                disabled={busy}
                onChange={(event) => updateDraft(event.target.value)}
                placeholder="Write or paste a Markdown skill…"
              />
            ) : (
              <div className="skill-diff-scroll min-h-0 flex-1 overflow-auto overscroll-contain px-6 py-5 [scrollbar-gutter:stable] max-md:p-3">
                {baselineQuery.isError ? (
                  <DSError>
                    {formatError(baselineQuery.error)}{" "}
                    <DSButton onClick={() => void baselineQuery.refetch()}>Retry comparison</DSButton>
                  </DSError>
                ) : baselineQuery.data === undefined ? (
                  <BrandLoading label="Loading default skill…" />
                ) : (
                  <SkillDiff rows={diffRows} />
                )}
              </div>
            )}
          </div>
          <footer className="skill-editor-footer flex min-h-13 flex-none items-center justify-between gap-3 border-t border-line bg-surface px-6 py-2.5 text-caption text-fg-muted [&_button]:flex-none max-md:flex-wrap">
            <span role="status">
              {notice || "Saving creates and activates a new version. The original stays unchanged."}
            </span>
            {dirty && (
              <DSButton
                tone="ghost"
                disabled={busy}
                onClick={() => {
                  if (window.confirm("Discard unsaved skill changes?")) {
                    setDraft(null);
                    setRevision((value) => value + 1);
                    setNotice("");
                  }
                }}
              >
                <RotateCcw className="size-3.5" />
                Discard changes
              </DSButton>
            )}
          </footer>
        </section>
      </div>
    </PlatformPage>
  );
}

type DiffRow = { kind: "same" | "add" | "remove"; oldLine: number | null; newLine: number | null; text: string };

function SkillDiff({ rows }: { rows: DiffRow[] }) {
  const changed = rows.some((row) => row.kind !== "same");
  if (!changed) return <DSEmptyState>No changes compared with Default.</DSEmptyState>;
  return (
    <div className="text-xs font-mono leading-relaxed">
      {rows.map((row) => (
        <div
          key={`${row.kind}:${row.oldLine}:${row.newLine}`}
          className={`grid grid-cols-[56px_56px_28px_minmax(0,1fr)] border-b border-[color-mix(in_srgb,var(--border)_55%,transparent)] ${row.kind === "add" ? "bg-emerald-50" : row.kind === "remove" ? "bg-rose-50" : ""}`}
        >
          <div className="select-none px-2 py-0.5 text-right text-fg-subtle">{row.oldLine ?? ""}</div>
          <div className="select-none border-l border-[color-mix(in_srgb,var(--border)_55%,transparent)] px-2 py-0.5 text-right text-fg-subtle">
            {row.newLine ?? ""}
          </div>
          <div className="select-none border-l border-[color-mix(in_srgb,var(--border)_55%,transparent)] px-2 py-0.5 text-fg-muted">
            {row.kind === "add" ? "+" : row.kind === "remove" ? "-" : ""}
          </div>
          <pre className="whitespace-pre-wrap break-words px-2 py-0.5 text-fg-muted">{row.text || " "}</pre>
        </div>
      ))}
    </div>
  );
}

function buildLineDiff(before: string, after: string): DiffRow[] {
  const a = before.split("\n");
  const b = after.split("\n");
  const dp = Array.from({ length: a.length + 1 }, () => Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--)
    for (let j = b.length - 1; j >= 0; j--)
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const rows: DiffRow[] = [];
  let i = 0,
    j = 0,
    oldLine = 1,
    newLine = 1;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      rows.push({ kind: "same", oldLine: oldLine++, newLine: newLine++, text: a[i++] });
      j++;
    } else if (j < b.length && (i === a.length || dp[i][j + 1] >= dp[i + 1][j]))
      rows.push({ kind: "add", oldLine: null, newLine: newLine++, text: b[j++] });
    else if (i < a.length) rows.push({ kind: "remove", oldLine: oldLine++, newLine: null, text: a[i++] });
  }
  return rows;
}

function formatSkillDate(value: number) {
  return new Date(value).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

function skillLabel(version: SkillVersion): string {
  if (version.id === 0) return "Default";
  return `v${version.version}`;
}

function SkillVersionRow({
  version,
  selected,
  busy,
  onSelect,
}: {
  version: SkillVersion;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      className={`review-process-item skill-version-row flex w-full cursor-pointer items-start gap-3 rounded-lg border p-3 text-left transition-colors ${selected ? "review-process-item-selected border-brand/40 bg-brand-soft/60 ring-1 ring-brand/20" : "border-transparent hover:border-line hover:bg-surface-muted"}`}
      aria-label={`Open ${skillLabel(version)}`}
      aria-pressed={selected}
      disabled={busy}
      onClick={onSelect}
    >
      <FileText className="mt-0.5 size-4 text-fg-muted" />
      <span className="min-w-0">
        <span className="skill-version-title flex flex-wrap items-center gap-2 text-small font-semibold text-fg">
          {skillLabel(version)}
          {version.active && (
            <span className="skill-active-badge inline-flex h-5 items-center rounded-full bg-emerald-50 px-2 text-caption font-medium text-emerald-700 ring-1 ring-emerald-600/20 ring-inset">
              Active
            </span>
          )}
        </span>
        <span
          className="skill-version-note mt-1 block truncate text-caption text-fg-muted"
          title={version.note ?? undefined}
        >
          {version.note ?? (version.id <= 0 ? "Built-in review skill" : "Review skill snapshot")}
        </span>
        <span className="skill-version-meta mt-1.5 block text-caption text-fg-subtle">
          {version.charCount.toLocaleString()} chars ·{" "}
          {version.id <= 0 ? "Built-in" : formatSkillDate(version.createdAt)}
        </span>
      </span>
    </button>
  );
}
