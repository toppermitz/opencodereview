import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Lock, Search } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { friendlyError } from "../components/AccessConnect";
import { BrandLoading } from "../components/BrandLoading";
import { cx } from "../components/DesignSystem";
import { setupApi } from "../services/setupApi";
import type { Repo } from "../types";
import { ctaPrimary, ctaSecondary, StepActions, StepHeader } from "./OnboardingParts";

const owner = (repo: Repo) => repo.fullName.split("/")[0] || repo.owner;

/** Every repository the GitLab sign-in can see, across all of its organizations, in one list. */
export function ReposStep({ onBack, onSaved }: { onBack: () => void; onSaved: (count: number) => void }) {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<Set<number> | null>(null);
  const [search, setSearch] = useState("");
  const [org, setOrg] = useState("all");
  const workspaces = useQuery({ queryKey: ["setup", "workspaces"], queryFn: () => setupApi.refreshWorkspaces() });
  const logins = (workspaces.data ?? []).map((workspace) => workspace.login);
  const repos = useQuery({
    queryKey: ["setup", "repos", logins.join("|")],
    // Refreshing the workspaces already pulled every repository from GitLab; read them back.
    queryFn: async () =>
      (await Promise.all(logins.map((login) => setupApi.listWorkspaceRepos(login).catch(() => [] as Repo[])))).flat(),
    enabled: logins.length > 0,
  });
  const all = useMemo(() => repos.data ?? [], [repos.data]);
  useEffect(() => {
    if (repos.data && selected === null)
      setSelected(new Set(repos.data.filter((repo) => repo.subscriptionEnabled).map((repo) => repo.id)));
  }, [repos.data, selected]);
  const chosen = selected ?? new Set<number>();
  const owners = [...new Set(all.map(owner))];
  const visible = all.filter(
    (repo) =>
      (org === "all" || owner(repo) === org) && (!search || repo.fullName.toLowerCase().includes(search.toLowerCase())),
  );
  const save = useMutation({
    mutationFn: async () => {
      await setupApi.saveSelectedWorkspaces([...new Set(all.filter((repo) => chosen.has(repo.id)).map(owner))]);
      // Only repositories whose state changed: each request is a round trip, and with hundreds of
      // repositories saving every one made the first save take minutes.
      const changed = all.filter((repo) => chosen.has(repo.id) !== repo.subscriptionEnabled);
      await Promise.all(changed.map((repo) => setupApi.saveRepoSubscription(repo, chosen.has(repo.id))));
      return chosen.size;
    },
    onSuccess: async (count) => {
      await queryClient.invalidateQueries({ queryKey: ["setup", "status"] });
      onSaved(count);
    },
  });
  const toggle = (id: number) =>
    setSelected((current) => {
      const next = new Set(current ?? []);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  const loading = workspaces.isLoading || (logins.length > 0 && repos.isLoading);
  const error = workspaces.error ?? repos.error ?? save.error;
  return (
    <>
      <StepHeader
        title="Pick your repositories"
        subtitle="OpenCodeReview indexes these and reviews their new merge requests."
      />
      <section className="flex min-h-0 flex-col gap-2.5" aria-label="Repositories">
        <div className="flex items-center gap-2 rounded-xl border border-line bg-surface-muted/60 px-3 focus-within:border-line-strong">
          <Search className="size-4 flex-none text-fg-subtle" aria-hidden="true" />
          <input
            className="h-10 w-full bg-transparent text-small text-fg outline-none placeholder:text-fg-subtle"
            value={search}
            placeholder="Search repositories"
            aria-label="Search repositories"
            onChange={(event) => setSearch(event.target.value)}
          />
        </div>
        {owners.length > 1 && (
          <div className="flex gap-1.5 overflow-x-auto [scrollbar-width:none]">
            {["all", ...owners].map((login) => (
              <button
                key={login}
                type="button"
                className={cx(
                  "h-8 flex-none cursor-pointer rounded-full px-3 text-caption font-semibold transition",
                  org === login ? "bg-fg text-canvas" : "bg-surface-muted text-fg-muted hover:text-fg",
                )}
                onClick={() => setOrg(login)}
              >
                {login === "all" ? `All · ${all.length}` : login}
              </button>
            ))}
          </div>
        )}
        <ul className="max-h-[min(46dvh,420px)] overflow-y-auto overscroll-contain rounded-2xl border border-line bg-surface-muted/60 [scrollbar-gutter:stable]">
          {loading ? (
            <li className="p-6">
              <BrandLoading label="Loading repositories…" />
            </li>
          ) : visible.length === 0 ? (
            <li className="p-6 text-center text-small text-fg-muted">
              {all.length ? "No repositories match your search." : "No repositories found for this GitLab account."}
            </li>
          ) : (
            visible.map((repo) => {
              const on = chosen.has(repo.id);
              return (
                <li key={repo.id} className="border-b border-line last:border-b-0">
                  {/* biome-ignore lint/a11y/useSemanticElements: custom-styled checkbox row; a native input would lose the full-row button layout and styling */}
                  <button
                    type="button"
                    role="checkbox"
                    aria-checked={on}
                    className="flex w-full cursor-pointer items-center gap-3 px-4 py-3 text-left transition hover:bg-surface-sunken/50"
                    onClick={() => toggle(repo.id)}
                  >
                    <span
                      className={cx(
                        "grid size-5 flex-none place-items-center rounded-md border",
                        on ? "border-transparent bg-brand text-brand-fg" : "border-line-strong",
                      )}
                      aria-hidden="true"
                    >
                      {on && <Check className="size-3.5 stroke-[3]" />}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-small font-semibold text-fg">{repo.fullName}</span>
                      <span className="block truncate text-caption text-fg-subtle">
                        {repo.defaultBranch ?? "default branch unknown"}
                      </span>
                    </span>
                    {repo.isPrivate && <Lock className="size-3.5 flex-none text-fg-subtle" aria-label="Private" />}
                  </button>
                </li>
              );
            })
          )}
        </ul>
      </section>
      {error && (
        <p className="text-center text-small font-semibold text-danger" role="alert">
          {friendlyError(error)}
        </p>
      )}
      <StepActions
        primary={
          <button
            type="button"
            className={ctaPrimary}
            disabled={chosen.size === 0 || save.isPending || loading}
            onClick={() => save.mutate()}
          >
            {save.isPending
              ? "Saving…"
              : chosen.size
                ? `Continue with ${chosen.size} repositor${chosen.size === 1 ? "y" : "ies"}`
                : "Continue"}
          </button>
        }
        secondary={
          <button type="button" className={ctaSecondary} onClick={onBack}>
            Back
          </button>
        }
        hint={
          chosen.size === 0 && !loading
            ? "Pick at least one repository to continue."
            : "You can change this later in Settings."
        }
      />
    </>
  );
}
