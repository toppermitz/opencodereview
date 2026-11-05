import { Search } from "lucide-react";
import { useState } from "react";
import { BrandLoading } from "../../../components/BrandLoading";
import { DSEmptyState, DSInput } from "../../../components/DesignSystem";
import type { Repo } from "../../../types";
import { repoOwner } from "../../lib/utils";

export function RepositorySelector({
  repos,
  selected,
  locked,
  disabled,
  loading,
  hasOrganizations,
  onToggle,
}: {
  repos: Repo[];
  selected: Set<number>;
  locked: Set<string>;
  disabled: boolean;
  loading: boolean;
  hasOrganizations: boolean;
  onToggle: (id: number) => void;
}) {
  const isSelected = (repo: Repo) => selected.has(repo.id) || locked.has(repoOwner(repo));
  const [search, setSearch] = useState("");
  const query = search.trim().toLowerCase();
  const filteredRepos = repos.filter((repo) => repo.fullName.toLowerCase().includes(query));
  return (
    <div className="settings-repository-selector flex min-w-0 flex-col gap-3">
      <div className="settings-multiselect-label flex items-center justify-between text-caption font-medium text-fg-muted [&>span:last-child]:text-fg-subtle">
        <span id="settings-repositories-label">Repositórios</span>
        <span>
          {repos.filter(isSelected).length} of {repos.length} selected
        </span>
      </div>
      <DSInput
        aria-label="Buscar repositórios"
        icon={<Search className="size-4" />}
        value={search}
        placeholder="Buscar por organização ou nome do repositório…"
        onChange={(event) => setSearch(event.target.value)}
        disabled={!hasOrganizations || loading}
      />
      {/* biome-ignore lint/a11y/useSemanticElements: scrollable list labelled by an external heading; a <fieldset> would need a <legend> and alter the scroll box */}
      <div
        className="settings-repository-list h-[max(27.5rem,calc(100dvh_-_26rem))] overflow-y-auto rounded-lg border border-line"
        role="group"
        aria-labelledby="settings-repositories-label"
        aria-busy={loading}
      >
        {!hasOrganizations ? (
          <DSEmptyState>Selecione organizações acima para ver os repositórios.</DSEmptyState>
        ) : loading ? (
          <BrandLoading size="compact" label="Carregando repositórios…" />
        ) : filteredRepos.length === 0 ? (
          <DSEmptyState>
            {query
              ? "Nenhum repositório corresponde à busca."
              : "Nenhum repositório disponível. Atualize o acesso para verificar de novo."}
          </DSEmptyState>
        ) : (
          filteredRepos.map((repo) => (
            <label
              key={repo.id}
              className={`settings-repository-option grid min-h-14 cursor-pointer grid-cols-[auto_minmax(0,1fr)_minmax(100px,auto)] items-center gap-3 border-b border-line px-3.5 py-3 last:border-b-0 hover:bg-surface-muted focus-within:outline-2 focus-within:outline-brand [&.selected]:bg-brand-soft/60 [&_input]:size-4 [&_input]:accent-brand${selected.has(repo.id) ? " selected" : ""}`}
            >
              <input
                type="checkbox"
                aria-label={repo.fullName}
                checked={isSelected(repo)}
                disabled={disabled || locked.has(repoOwner(repo))}
                onChange={() => onToggle(repo.id)}
              />
              <span className="settings-repository-name text-small font-medium text-fg [overflow-wrap:anywhere]">
                {repo.fullName}
              </span>
              <span className="settings-repository-meta max-w-56 text-right text-caption text-fg-subtle [overflow-wrap:anywhere]">
                {repo.defaultBranch || "Branch desconhecido"} · {repo.isPrivate ? "Privado" : "Público"}
              </span>
            </label>
          ))
        )}
      </div>
    </div>
  );
}
