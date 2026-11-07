import { BrandLoading } from "../../../components/BrandLoading";
import { SettingChoice, SettingRow, SettingsGroup, SettingsSection, TokenEditor } from "./SettingsControls";
import type { SettingsForm, SettingsSectionId } from "./settingsForm";

export function ReviewSettingsPanel({
  activeSection,
  settings,
  loading,
  onSave,
  keywordDraft,
  onKeywordDraft,
}: {
  activeSection: SettingsSectionId;
  settings?: SettingsForm;
  loading: boolean;
  onSave: (key: string, value: string) => void;
  keywordDraft: string;
  onKeywordDraft: (value: string) => void;
}) {
  if (!settings)
    return activeSection === "selection" ? <BrandLoading label="Carregando configurações de revisão…" /> : null;
  const setArray = (key: string, items: string[]) =>
    onSave(key, JSON.stringify(items.map((item) => item.trim()).filter(Boolean)));
  return (
    <SettingsSection id="selection" activeSection={activeSection}>
      <SettingsGroup title="Seleção de MRs" description="Controla quais MRs entram na fila de revisão.">
        <SettingChoice
          title="Escopo da revisão"
          description="Use assignments e revisores do GitLab, ou revise todos os MRs abertos."
          options={["true", "false"]}
          labels={{ true: "Atribuídos", false: "Todos os MRs" }}
          value={String(settings.reviewAssignedOnly)}
          disabled={loading}
          onChange={(value) => onSave("review_assigned_only", value)}
        />
        <SettingRow
          title="Máximo de arquivos do MR"
          description="Pula MRs que alteram mais arquivos que este limite. O padrão é 100; exclusões contam; saída de build, vendored e gerados não."
        >
          <input
            className="settings-number-input h-9 w-24 rounded-md border border-line bg-surface px-3 text-small text-fg shadow-card outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
            type="number"
            aria-label="Máximo de arquivos do MR"
            min={1}
            value={settings.reviewMaxFiles}
            disabled={loading}
            onChange={(event) => onSave("review_max_files", event.target.value)}
          />
        </SettingRow>
        <SettingRow
          title="Revisões ao mesmo tempo"
          description="Quantos merge requests são revisados de uma vez. O padrão é 20; cada revisão roda em seu próprio container isolado."
        >
          <input
            className="settings-number-input h-9 w-24 rounded-md border border-line bg-surface px-3 text-small text-fg shadow-card outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
            type="number"
            aria-label="Revisões ao mesmo tempo"
            min={1}
            max={64}
            value={settings.reviewConcurrency}
            disabled={loading}
            onChange={(event) => onSave("review_concurrency", event.target.value)}
          />
        </SettingRow>
        <SettingChoice
          title="Intervalo de poll"
          description="Com que frequência o OpenCodeReview verifica os repositórios selecionados."
          options={["30", "60", "300", "1800", "3600"]}
          labels={{ "30": "30s", "60": "1m", "300": "5m", "1800": "30m", "3600": "1h" }}
          value={String(settings.pollIntervalSeconds)}
          disabled={loading}
          onChange={(value) => onSave("poll_interval_seconds", value)}
        />
      </SettingsGroup>
      <SettingsGroup title="Comentários da revisão" description="Escolha o que a revisão publica além dos achados.">
        <SettingChoice
          title="Publicar sugestões"
          description="Notas de menor prioridade, como testes, docs e nomes, listadas numa seção recolhida do resumo. Desligado publica só achados e perguntas."
          options={["true", "false"]}
          labels={{ true: "Ligado", false: "Desligado" }}
          value={String(settings.reviewSuggestionsEnabled)}
          disabled={loading}
          onChange={(value) => onSave("review_suggestions_enabled", value)}
        />
      </SettingsGroup>
      <SettingsGroup
        title="Aprovação automática"
        description="Quando a revisão não encontra achados, pode aprovar o MR no GitLab sozinha se o risco couber no limite."
      >
        <SettingChoice
          title="Auto-aprovar MRs limpos"
          description="Publica /approve no GitLab quando não há achados e o risco está no máximo permitido. Desligado deixa a decisão no carrossel (Aprovar, Marcar como revisado ou Ignorar)."
          options={["true", "false"]}
          labels={{ true: "Ligado", false: "Desligado" }}
          value={String(settings.autoApproveEnabled)}
          disabled={loading}
          onChange={(value) => onSave("auto_approve_enabled", value)}
        />
        <SettingRow
          title="Risco máximo para auto-aprovar"
          description="Só auto-aprova se o score de risco da execução for menor ou igual a este valor. Sem score, não auto-aprova. O padrão é 20."
        >
          <input
            className="settings-number-input h-9 w-24 rounded-md border border-line bg-surface px-3 text-small text-fg shadow-card outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
            type="number"
            aria-label="Risco máximo para auto-aprovar"
            min={0}
            value={settings.autoApproveMaxRiskScore}
            disabled={loading || !settings.autoApproveEnabled}
            onChange={(event) => onSave("auto_approve_max_risk_score", event.target.value)}
          />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Filtros de título" description="Pula MRs de baixo valor por palavra-chave.">
        <TokenEditor
          label="Palavras-chave ignoradas"
          placeholder="renovate, chore(deps)"
          values={settings.autoSkipKeywords}
          draft={keywordDraft}
          onDraft={onKeywordDraft}
          disabled={loading}
          onChange={(items) =>
            setArray(
              "auto_skip_keywords",
              items.map((item) => item.toLowerCase()),
            )
          }
        />
      </SettingsGroup>
    </SettingsSection>
  );
}
