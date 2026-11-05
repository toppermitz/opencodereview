import type { IndexJob } from "../../services/setupApi";
import type { RepoIndexState, RepoIndexStatus } from "../../types";

export const indexGroups: Array<{ title: string; states: RepoIndexState[] }> = [
  { title: "Precisa de atenção", states: ["failed", "stale", "cancelled"] },
  { title: "Em andamento", states: ["indexing", "queued"] },
  { title: "Pronto", states: ["ready"] },
  { title: "Não indexado", states: ["not_indexed", "empty"] },
];

export const indexStateLabel: Record<RepoIndexState, string> = {
  ready: "Pronto",
  queued: "Na fila",
  indexing: "Indexando",
  stale: "Desatualizado",
  failed: "Falhou",
  cancelled: "Cancelado",
  not_indexed: "Não indexado",
  empty: "Sem commits ainda",
};

export function selectRepository(jobs: IndexJob[], current: number | null) {
  return (
    jobs.find((job) => job.repo.id === current)?.repo.id ??
    jobs.find((job) => job.status.status === "ready")?.repo.id ??
    jobs[0]?.repo.id ??
    null
  );
}

export function indexRevision(status: Pick<RepoIndexStatus, "indexedAt" | "baseSha">) {
  return status.indexedAt === null ? null : `${status.indexedAt}:${status.baseSha ?? ""}`;
}

interface IndexingWorkspaceState {
  listOpen: boolean;
  restoreList: boolean;
  nodeId: string | null;
}
type IndexingWorkspaceAction =
  | { type: "inspect"; nodeId: string }
  | { type: "close" | "toggle-list" }
  | { type: "reset" | "resize"; wide: boolean };

export function indexingWorkspaceReducer(
  state: IndexingWorkspaceState,
  action: IndexingWorkspaceAction,
): IndexingWorkspaceState {
  switch (action.type) {
    case "inspect":
      return {
        nodeId: action.nodeId,
        listOpen: false,
        restoreList: state.nodeId === null ? state.listOpen : state.restoreList,
      };
    case "close":
      return { ...state, nodeId: null, listOpen: state.restoreList };
    case "toggle-list":
      return { nodeId: null, listOpen: !state.listOpen, restoreList: !state.listOpen };
    case "reset":
      return { nodeId: null, listOpen: action.wide, restoreList: action.wide };
    case "resize":
      return { ...state, listOpen: state.nodeId === null && action.wide, restoreList: action.wide };
  }
}
