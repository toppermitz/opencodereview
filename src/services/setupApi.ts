import type { GitLabAuthStatus, Repo, Workspace } from "../types";
import { setupApiAccess } from "./setupApiAccess";
import { setupApiReviews } from "./setupApiReviews";
import { setupApiUsage } from "./setupApiUsage";
import { setupApiWorkspace } from "./setupApiWorkspace";

export type { IndexJob, IndexJobsResponse } from "./setupApiTransport";

export interface SetupStatus {
  auth: GitLabAuthStatus;
  workspaces: Workspace[];
  selectedWorkspaces: Workspace[];
  selectedRepos: Repo[];
  complete: boolean;
  /** 1 GitLab, 2 AI plan, 3 repositories, 4 done. */
  nextStep: 1 | 2 | 3 | 4;
}

export const setupApi = {
  ...setupApiAccess,
  ...setupApiReviews,
  ...setupApiUsage,
  ...setupApiWorkspace,
  async setupStatus(): Promise<SetupStatus> {
    const auth = await setupApiAccess.gitlabAuthStatus();
    if (!auth.loggedIn)
      return { auth, workspaces: [], selectedWorkspaces: [], selectedRepos: [], complete: false, nextStep: 1 };
    const providers = await setupApiAccess.aiProviders().catch(() => []);
    if (!providers.some((provider) => provider.ready > 0))
      return { auth, workspaces: [], selectedWorkspaces: [], selectedRepos: [], complete: false, nextStep: 2 };
    const workspaces = await setupApiWorkspace.listWorkspaces().catch(() => []);
    const selectedWorkspaces = workspaces.filter((workspace) => workspace.selected);
    if (selectedWorkspaces.length === 0)
      return { auth, workspaces, selectedWorkspaces, selectedRepos: [], complete: false, nextStep: 3 };
    const repos = (
      await Promise.all(
        selectedWorkspaces.map((workspace) => setupApiWorkspace.listWorkspaceRepos(workspace.login).catch(() => [])),
      )
    ).flat();
    const selectedRepos = repos.filter((repo) => repo.subscriptionEnabled);
    return {
      auth,
      workspaces,
      selectedWorkspaces,
      selectedRepos,
      complete: selectedRepos.length > 0,
      nextStep: selectedRepos.length > 0 ? 4 : 3,
    };
  },
};
