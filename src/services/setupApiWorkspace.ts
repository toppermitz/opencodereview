import type { Repo, SkillVersion, Workspace } from "../types";
import { http } from "./setupApiTransport";
export const setupApiWorkspace = {
  listSkillVersions: () => http<SkillVersion[]>("/skills/versions"),
  getSkillVersionContent: (id: number) =>
    http<{ content: string }>(`/skills/versions/${id}/content`).then((value) => value.content),
  setActiveSkillVersion: (id: number) => http<SkillVersion[]>(`/skills/versions/${id}/activate`, { method: "POST" }),
  createSkillVersion: (content: string) =>
    http<SkillVersion>("/skills/versions", { method: "POST", body: JSON.stringify({ content, note: null }) }),
  deleteSkillVersion: (id: number) => http<SkillVersion[]>(`/skills/versions/${id}`, { method: "DELETE" }),
  listWorkspaces: () => http<Workspace[]>("/workspaces"),
  refreshWorkspaces: () => http<Workspace[]>("/gitlab/workspaces"),
  saveSelectedWorkspaces: (logins: string[]) =>
    http<Workspace[]>("/workspaces/select", { method: "POST", body: JSON.stringify({ logins }) }),
  listWorkspaceRepos: (login: string) => http<Repo[]>(`/workspaces/${encodeURIComponent(login)}/repos`),
  refreshWorkspaceRepos: (login: string) =>
    http<Repo[]>(`/workspaces/${encodeURIComponent(login)}/repos/refresh`, { method: "POST" }),
  saveRepoSubscription: (repo: Repo, enabled: boolean) =>
    http<Repo>(`/repos/${repo.id}/subscribe`, { method: "POST", body: JSON.stringify({ enabled }) }),
};
