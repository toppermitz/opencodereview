import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useBlocker, useNavigate } from "@tanstack/react-router";
import { RefreshCw } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { DSButton, DSEmptyState, DSError } from "../../components/DesignSystem";
import { toast } from "../../components/Toast";
import { setupApi } from "../../services/setupApi";
import type { Repo } from "../../types";
import { MultiSelect } from "../components/MultiSelect";
import { PlatformPage } from "../components/PlatformHeader";
import { ProvidersPanel } from "../components/ProvidersPanel";
import { formatError, repoOwner, toggleSet } from "../lib/utils";
import { GitLabAuthRecovery } from "./settings/GitLabAuthRecovery";
import { RepositorySelector } from "./settings/RepositorySelector";
import { ReviewSettingsPanel } from "./settings/ReviewSettingsPanel";
import { IntegrationRow, SettingsGroup, SettingsSection } from "./settings/SettingsControls";
import {
  SETTINGS_SECTIONS,
  type SettingsForm,
  type SettingsSectionId,
  serializeSetting,
  settingField,
  settingSection,
} from "./settings/settingsForm";

export function SettingsPage() {
  const [activeSection, setActiveSection] = useState<SettingsSectionId>(() => {
    const requested = new URLSearchParams(window.location.search).get("section");
    return SETTINGS_SECTIONS.find((section) => section.id === requested)?.id ?? "accounts";
  });
  const contentRef = useRef<HTMLDivElement>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: activeSection is a trigger: scroll to the top when the section changes
  useEffect(() => {
    if (contentRef.current) contentRef.current.scrollTop = 0;
  }, [activeSection]);
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const statusQuery = useQuery({ queryKey: ["setup", "status"], queryFn: () => setupApi.setupStatus() });
  const reviewSettingsQuery = useQuery({
    queryKey: ["setup", "settings", "review"],
    queryFn: () => setupApi.getSettings(),
  });
  const [authFlowStarted, setAuthFlowStarted] = useState(false);
  const authEventsQuery = useQuery({
    queryKey: ["setup", "settings", "auth-events"],
    queryFn: () => setupApi.gitlabAuthEvents(),
    enabled: Boolean(statusQuery.data && (!statusQuery.data.auth.loggedIn || authFlowStarted)),
    refetchInterval: (query) => (query.state.data?.running ? 1000 : false),
  });
  const [selectedLogins, setSelectedLogins] = useState<Set<string>>(new Set());
  const [selectedRepoIds, setSelectedRepoIds] = useState<Set<number>>(new Set());
  const [monitorAllLogins, setMonitorAllLogins] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);
  const [settingDraft, setSettingDraft] = useState<Record<string, string>>({});
  const [keywordDraft, setKeywordDraft] = useState("");
  const [draftRevision, setDraftRevision] = useState(0);
  const [notice, setNotice] = useState("");
  const saving = useRef(false);
  const savedSettings = reviewSettingsQuery.data;
  const settings = useMemo(
    () =>
      savedSettings &&
      (Object.assign(
        {},
        savedSettings,
        ...Object.entries(settingDraft).map(([key, value]) => {
          const field = settingField(key);
          const original = savedSettings[field];
          return {
            [field]:
              typeof original === "boolean"
                ? value === "true"
                : typeof original === "object"
                  ? JSON.parse(value)
                  : value,
          };
        }),
      ) as SettingsForm | undefined),
    [savedSettings, settingDraft],
  );
  const settingChanges = Object.fromEntries(
    Object.entries(settingDraft).filter(
      ([key, value]) => value !== serializeSetting(savedSettings?.[settingField(key)]),
    ),
  );
  const pendingKeyword = keywordDraft.trim().toLowerCase();
  if (pendingKeyword && settings && !settings.autoSkipKeywords.includes(pendingKeyword))
    settingChanges.auto_skip_keywords = JSON.stringify([...settings.autoSkipKeywords, pendingKeyword]);
  const changeSetting = (key: string, value: string) => {
    if (saving.current) return;
    setSettingDraft((current) => {
      const next = { ...current };
      if (value === serializeSetting(savedSettings?.[settingField(key)])) delete next[key];
      else next[key] = value;
      return next;
    });
    setNotice("");
    setError(null);
  };

  const selectableWorkspaces = useMemo(
    () => (statusQuery.data?.workspaces ?? []).filter((workspace) => workspace.repoCount > 0),
    [statusQuery.data?.workspaces],
  );
  const selectedLoginKey = [...selectedLogins].sort().join("|");
  const reposQueryKey = ["setup", "settings", "repos", selectedLoginKey] as const;
  const reposQuery = useQuery({
    queryKey: reposQueryKey,
    queryFn: async () =>
      (await Promise.all([...selectedLogins].map((login) => setupApi.listWorkspaceRepos(login)))).flat(),
    enabled: selectedLogins.size > 0 && Boolean(statusQuery.data?.auth.loggedIn),
  });
  const refreshWorkspaces = useMutation({
    mutationFn: () => setupApi.refreshWorkspaces(),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["setup", "status"] }),
    onError: (cause) => setError(formatError(cause)),
  });
  const workspacesMissing =
    Boolean(statusQuery.data?.auth.loggedIn) &&
    !statusQuery.data?.workspaces.some((workspace) => workspace.repoCount > 0);
  useEffect(() => {
    if (workspacesMissing && refreshWorkspaces.isIdle) refreshWorkspaces.mutate();
  }, [workspacesMissing, refreshWorkspaces]);
  const refreshRepos = useMutation({
    mutationFn: async () =>
      (await Promise.all([...selectedLogins].map((login) => setupApi.refreshWorkspaceRepos(login)))).flat(),
    onSuccess: (repos) => {
      queryClient.setQueryData(reposQueryKey, repos);
      setError(null);
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const connectGitLab = useMutation({
    mutationFn: (action: "login" | "refresh") => setupApi.gitlabAuthStart(action),
    onSuccess: () => {
      setError(null);
      setAuthFlowStarted(true);
      void queryClient.invalidateQueries({ queryKey: ["setup", "settings", "auth-events"] });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const signOut = useMutation({
    mutationFn: () => setupApi.gitlabAuthLogout(),
    onSuccess: async () => {
      setError(null);
      setAuthFlowStarted(false);
      await queryClient.invalidateQueries({ queryKey: ["setup"] });
      await navigate({ to: "/" });
    },
    onError: (cause) => setError(formatError(cause)),
  });
  const confirmSignOut = () => {
    if (window.confirm("Sign out of GitLab? Reviews stop until you connect again.")) signOut.mutate();
  };
  const save = useMutation({
    mutationFn: setupApi.saveSettingsBatch,
    onSuccess: ({ settings: saved, workspaces, selectedRepos }) => {
      setError(null);
      queryClient.setQueryData(["setup", "settings", "review"], saved);
      queryClient.setQueryData(["setup", "status"], (current: typeof statusQuery.data) =>
        current
          ? {
              ...current,
              workspaces,
              selectedWorkspaces: workspaces.filter((workspace) => workspace.selected),
              selectedRepos,
            }
          : current,
      );
      queryClient.setQueriesData<Repo[]>({ queryKey: ["setup", "settings", "repos"] }, (current) =>
        current?.map((repo) => ({
          ...repo,
          subscriptionEnabled: selectedRepos.some((selected) => selected.id === repo.id),
        })),
      );
      setSelectedLogins(
        new Set(
          workspaces
            .filter((workspace) => workspace.selected && workspace.repoCount > 0)
            .map((workspace) => workspace.login),
        ),
      );
      setSelectedRepoIds(new Set(selectedRepos.map((repo) => repo.id)));
      setMonitorAllLogins(
        new Set(
          workspaces
            .filter((workspace) => workspace.selected && workspace.monitorAll)
            .map((workspace) => workspace.login),
        ),
      );
      setSettingDraft({});
      setKeywordDraft("");
      setDraftRevision((value) => value + 1);
      setInitialized(true);
      setNotice("All changes saved.");
      toast("Changes saved");
      for (const queryKey of [
        ["setup", "status"],
        ["indexing", "jobs"],
        ["reviews", "poll", "status"],
      ])
        void queryClient.invalidateQueries({ queryKey });
    },
    onError: (cause) => {
      setError(`Could not save changes. Your edits are still pending. ${formatError(cause)}`);
      setNotice("");
    },
    onSettled: () => {
      saving.current = false;
    },
  });

  useEffect(() => {
    if (!statusQuery.data || initialized) return;
    setSelectedLogins(
      new Set(
        statusQuery.data.selectedWorkspaces
          .filter((workspace) => workspace.repoCount > 0)
          .map((workspace) => workspace.login),
      ),
    );
    setSelectedRepoIds(new Set(statusQuery.data.selectedRepos.map((repo) => repo.id)));
    setMonitorAllLogins(
      new Set(
        statusQuery.data.selectedWorkspaces
          .filter((workspace) => workspace.monitorAll)
          .map((workspace) => workspace.login),
      ),
    );
    setInitialized(true);
  }, [initialized, statusQuery.data]);

  const status = statusQuery.data;
  const authDisconnected = Boolean(status && !status.auth.loggedIn);
  const authUsesEnvToken = Boolean(status?.auth.detail.includes("GITLAB_TOKEN"));
  const authPrompt: { code?: string; url?: string } = Object.assign(
    {},
    ...(authEventsQuery.data?.events ?? []).filter((event) => event.kind === "prompt"),
  );
  const authDone = (authEventsQuery.data?.events ?? []).find((event) => event.kind === "done");
  const workspaces = selectableWorkspaces;
  const repos = reposQuery.data ?? [];
  const sortedRepos = useMemo(
    () =>
      [...repos].sort((left, right) => {
        const leftSelected = selectedRepoIds.has(left.id);
        const rightSelected = selectedRepoIds.has(right.id);
        if (leftSelected !== rightSelected) return leftSelected ? -1 : 1;
        return left.fullName.localeCompare(right.fullName);
      }),
    [repos, selectedRepoIds],
  );
  const activeMonitorAll = useMemo(
    () => new Set([...monitorAllLogins].filter((login) => selectedLogins.has(login))),
    [monitorAllLogins, selectedLogins],
  );
  const repoSelected = (repo: Repo) => selectedRepoIds.has(repo.id) || activeMonitorAll.has(repoOwner(repo));
  const visibleSelectedCount = repos.filter(repoSelected).length;
  const storedMonitorAll =
    status?.selectedWorkspaces.filter((workspace) => workspace.monitorAll).map((workspace) => workspace.login) ?? [];
  const monitorAllDirty =
    activeMonitorAll.size !== storedMonitorAll.length || storedMonitorAll.some((login) => !activeMonitorAll.has(login));
  const storedSelectedWorkspaces = status?.selectedWorkspaces.filter((workspace) => workspace.repoCount > 0) ?? [];
  const repositoriesDirty =
    initialized && status
      ? selectedLogins.size !== storedSelectedWorkspaces.length ||
        selectedRepoIds.size !== status.selectedRepos.length ||
        storedSelectedWorkspaces.some((workspace) => !selectedLogins.has(workspace.login)) ||
        status.selectedRepos.some((repo) => !selectedRepoIds.has(repo.id)) ||
        monitorAllDirty
      : false;
  const dirtySections = new Set(Object.keys(settingChanges).map(settingSection));
  if (repositoriesDirty) dirtySections.add("repositories");
  if (keywordDraft) dirtySections.add("selection");
  const dirty = dirtySections.size > 0;
  useBlocker({
    shouldBlockFn: () => save.isPending || (dirty && !window.confirm("Discard unsaved settings and leave this page?")),
    enableBeforeUnload: dirty || save.isPending,
  });
  const loading =
    statusQuery.isLoading ||
    reposQuery.isLoading ||
    refreshWorkspaces.isPending ||
    refreshRepos.isPending ||
    connectGitLab.isPending ||
    save.isPending;
  const workspaceOptions = workspaces.map((workspace) => ({
    value: workspace.login,
    label: workspace.login,
    detail: `${workspace.kind === "user" ? "Personal account" : "Organization"} · ${workspace.repoCount} repos`,
  }));
  const resetRepositorySelection = () => {
    if (!status) return;
    setSelectedLogins(
      new Set(
        status.selectedWorkspaces.filter((workspace) => workspace.repoCount > 0).map((workspace) => workspace.login),
      ),
    );
    setSelectedRepoIds(new Set(status.selectedRepos.map((repo) => repo.id)));
    setMonitorAllLogins(
      new Set(
        status.selectedWorkspaces.filter((workspace) => workspace.monitorAll).map((workspace) => workspace.login),
      ),
    );
  };
  const cancelChanges = () => {
    if (saving.current) return;
    resetRepositorySelection();
    setSettingDraft({});
    setKeywordDraft("");
    setDraftRevision((value) => value + 1);
    setError(null);
    setNotice("");
  };
  const saveChanges = () => {
    if (saving.current || saveDisabled) return;
    if (!Object.keys(settingChanges).length && !repositoriesDirty) return;
    saving.current = true;
    const available = [...repos, ...(status?.selectedRepos ?? [])];
    const repoIds = [...selectedRepoIds].filter((id) =>
      available.some((repo) => repo.id === id && selectedLogins.has(repoOwner(repo))),
    );
    save.mutate({
      settings: settingChanges,
      ...(repositoriesDirty
        ? { repositories: { logins: [...selectedLogins], repoIds, monitorAll: [...activeMonitorAll] } }
        : {}),
    });
  };
  const saveDisabled =
    !dirty ||
    loading ||
    !savedSettings ||
    (repositoriesDirty && (authDisconnected || reposQuery.isFetching || reposQuery.isError));
  const refreshRepositoryAccess = () => {
    refreshWorkspaces.mutate();
    if (selectedLogins.size > 0) refreshRepos.mutate();
  };

  useEffect(() => {
    if (!authDone?.success) return;
    void statusQuery.refetch();
    setAuthFlowStarted(false);
  }, [authDone?.success, statusQuery.refetch]);

  return (
    <PlatformPage
      title="Settings"
      description="Configure repositories, review behavior, and external integrations."
      action={
        <div className="settings-global-actions flex flex-wrap items-center gap-2 [&>span]:mr-2 [&>span]:text-caption [&>span]:text-fg-subtle [&>span:empty]:hidden">
          <span role="status">{dirty ? "Unsaved changes" : notice}</span>
          <DSButton onClick={cancelChanges} disabled={!dirty || save.isPending}>
            Cancel
          </DSButton>
          <DSButton tone="primary" onClick={saveChanges} disabled={saveDisabled}>
            {save.isPending ? "Saving…" : "Save changes"}
          </DSButton>
        </div>
      }
    >
      <div className="review-workspace settings-workspace flex min-h-0 flex-1 border-t border-line bg-canvas max-md:flex-col">
        <aside className="settings-sidebar w-60 shrink-0 overflow-y-auto border-r border-line bg-surface px-3 py-5 max-md:w-auto max-md:border-r-0 max-md:border-b [&>h2]:mx-3 [&>h2]:mb-3 [&>h2]:text-overline [&>h2]:text-fg-subtle [&>h2]:uppercase [&>nav]:flex [&>nav]:flex-col [&>nav]:gap-1 max-md:[&>nav]:flex-row max-md:[&>nav]:overflow-x-auto">
          <h2>Sections</h2>
          <nav aria-label="Settings sections">
            {SETTINGS_SECTIONS.map(({ id, title, icon: Icon }) => (
              <button
                key={id}
                id={`settings-nav-${id}`}
                type="button"
                className="settings-nav-item flex w-full cursor-pointer items-center gap-2.5 rounded-md px-3 py-2 text-left text-small font-medium text-fg-muted transition-colors hover:bg-surface-muted hover:text-fg aria-[current=true]:bg-brand-soft aria-[current=true]:text-brand [&>svg]:shrink-0 max-md:w-auto max-md:flex-none"
                aria-current={activeSection === id ? "true" : undefined}
                aria-controls={`settings-${id}`}
                onClick={() => setActiveSection(id)}
              >
                <Icon className="size-4" />
                <span>{title}</span>
                {dirtySections.has(id) && (
                  <span
                    className="settings-unsaved-dot ml-auto size-1.5 shrink-0 rounded-full bg-amber-500"
                    role="img"
                    aria-label="Unsaved changes"
                  />
                )}
              </button>
            ))}
          </nav>
        </aside>
        <div
          ref={contentRef}
          className="settings-workspace-main flex min-h-0 min-w-0 flex-1 flex-col gap-5 overflow-y-auto px-8 py-8 [scrollbar-gutter:stable] max-md:px-4 [&>*]:mx-auto [&>*]:w-full [&>*]:max-w-3xl"
        >
          {error && <DSError>{error}</DSError>}
          {(reviewSettingsQuery.error || statusQuery.error || reposQuery.error) && (
            <DSError>{formatError(reviewSettingsQuery.error ?? statusQuery.error ?? reposQuery.error)}</DSError>
          )}
          {authDisconnected && activeSection !== "integrations" && (
            <GitLabAuthRecovery
              prompt={authPrompt}
              doneMessage={authDone?.message}
              doneSuccess={authDone?.success}
              loading={connectGitLab.isPending}
              onConnect={() => connectGitLab.mutate("login")}
              onRefresh={() => void statusQuery.refetch()}
            />
          )}
          <SettingsSection id="accounts" activeSection={activeSection}>
            <ProvidersPanel />
          </SettingsSection>
          <SettingsSection id="repositories" activeSection={activeSection}>
            <SettingsGroup
              title="Repository access"
              description="Select organizations first, then choose monitored repositories."
              action={
                <div className="flex items-center gap-3">
                  <span className="text-xs font-medium text-fg-muted">{visibleSelectedCount} repos selected</span>
                  <DSButton onClick={refreshRepositoryAccess} disabled={loading || authDisconnected}>
                    <RefreshCw className="size-3.5" />
                    Refresh
                  </DSButton>
                </div>
              }
            >
              {authDisconnected ? (
                <DSEmptyState>Reconnect GitLab to manage repositories.</DSEmptyState>
              ) : (
                <div className="settings-select-stack flex flex-col gap-3.5 p-4">
                  <MultiSelect
                    label="Organizations"
                    placeholder="Select organizations"
                    options={workspaceOptions}
                    selected={selectedLogins}
                    onToggle={(login) => setSelectedLogins((current) => toggleSet(current, login))}
                    disabled={loading}
                  />
                  {selectedLogins.size > 0 && (
                    // biome-ignore lint/a11y/useSemanticElements: a <fieldset> adds UA min-inline-size and border quirks to this flex list
                    <div
                      className="settings-monitor-all flex flex-col gap-2"
                      role="group"
                      aria-label="Monitor all repositories"
                    >
                      {[...selectedLogins].sort().map((login) => (
                        <label key={login} className="flex cursor-pointer items-center gap-2.5 text-small text-fg">
                          <input
                            type="checkbox"
                            aria-label={`Monitor all ${login} repositories`}
                            checked={monitorAllLogins.has(login)}
                            disabled={loading}
                            onChange={() => setMonitorAllLogins((current) => toggleSet(current, login))}
                          />
                          <span>
                            All <strong className="font-semibold">{login}</strong> repositories, including ones created
                            later
                          </span>
                        </label>
                      ))}
                    </div>
                  )}
                  <RepositorySelector
                    repos={sortedRepos}
                    selected={selectedRepoIds}
                    locked={activeMonitorAll}
                    onToggle={(id) => setSelectedRepoIds((current) => toggleSet(current, id))}
                    disabled={loading}
                    loading={reposQuery.isLoading}
                    hasOrganizations={selectedLogins.size > 0}
                  />
                </div>
              )}
            </SettingsGroup>
          </SettingsSection>
          <ReviewSettingsPanel
            key={draftRevision}
            activeSection={activeSection}
            settings={settings}
            loading={reviewSettingsQuery.isLoading || save.isPending}
            onSave={changeSetting}
            keywordDraft={keywordDraft}
            onKeywordDraft={setKeywordDraft}
          />
          <SettingsSection id="integrations" activeSection={activeSection}>
            <SettingsGroup title="Integrations" description="Connected services used by OpenCodeReview.">
              {status ? (
                <>
                  <IntegrationRow
                    title="GitLab"
                    detail={
                      status.auth.loggedIn
                        ? `${status.auth.login ?? "GitLab CLI"} · ${authUsesEnvToken ? "GITLAB_TOKEN from environment" : status.auth.host}`
                        : "Not connected"
                    }
                    connected={status.auth.loggedIn}
                    action={
                      status.auth.loggedIn ? (
                        !authUsesEnvToken && (
                          <DSButton onClick={() => confirmSignOut()} disabled={loading || signOut.isPending}>
                            Sign out
                          </DSButton>
                        )
                      ) : (
                        <DSButton tone="primary" onClick={() => connectGitLab.mutate("login")} disabled={loading}>
                          {connectGitLab.isPending ? "Starting" : "Connect GitLab"}
                        </DSButton>
                      )
                    }
                  />
                  {!status.auth.loggedIn && (
                    <GitLabAuthRecovery
                      prompt={authPrompt}
                      doneMessage={authDone?.message}
                      doneSuccess={authDone?.success}
                      loading={connectGitLab.isPending}
                      onConnect={() => connectGitLab.mutate("login")}
                      onRefresh={() => void statusQuery.refetch()}
                    />
                  )}
                </>
              ) : (
                <DSEmptyState>Checking access…</DSEmptyState>
              )}
            </SettingsGroup>
          </SettingsSection>
        </div>
      </div>
    </PlatformPage>
  );
}
