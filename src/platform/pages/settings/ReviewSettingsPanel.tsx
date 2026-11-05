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
  if (!settings) return activeSection === "selection" ? <BrandLoading label="Loading review settings…" /> : null;
  const setArray = (key: string, items: string[]) =>
    onSave(key, JSON.stringify(items.map((item) => item.trim()).filter(Boolean)));
  return (
    <SettingsSection id="selection" activeSection={activeSection}>
      <SettingsGroup title="PR selection" description="Control which PRs enter the review queue.">
        <SettingChoice
          title="Review scope"
          description="Use assignments and GitLab reviewers, or review all open MRs."
          options={["true", "false"]}
          labels={{ true: "Assigned", false: "All PRs" }}
          value={String(settings.reviewAssignedOnly)}
          disabled={loading}
          onChange={(value) => onSave("review_assigned_only", value)}
        />
        <SettingRow
          title="Maximum PR files"
          description="Skip PRs that change more files than this limit. The default is 100; deletions count, build output, vendored and generated files do not."
        >
          <input
            className="settings-number-input h-9 w-24 rounded-md border border-line bg-surface px-3 text-small text-fg shadow-card outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
            type="number"
            aria-label="Maximum PR files"
            min={1}
            value={settings.reviewMaxFiles}
            disabled={loading}
            onChange={(event) => onSave("review_max_files", event.target.value)}
          />
        </SettingRow>
        <SettingRow
          title="Reviews at a time"
          description="How many merge requests are reviewed at once. The default is 20; each review runs in its own isolated container."
        >
          <input
            className="settings-number-input h-9 w-24 rounded-md border border-line bg-surface px-3 text-small text-fg shadow-card outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
            type="number"
            aria-label="Reviews at a time"
            min={1}
            max={64}
            value={settings.reviewConcurrency}
            disabled={loading}
            onChange={(event) => onSave("review_concurrency", event.target.value)}
          />
        </SettingRow>
        <SettingChoice
          title="Poll interval"
          description="How often OpenCodeReview checks selected repos."
          options={["30", "60", "300", "1800", "3600"]}
          labels={{ "30": "30s", "60": "1m", "300": "5m", "1800": "30m", "3600": "1h" }}
          value={String(settings.pollIntervalSeconds)}
          disabled={loading}
          onChange={(value) => onSave("poll_interval_seconds", value)}
        />
      </SettingsGroup>
      <SettingsGroup title="Review comments" description="Choose what a review posts besides its findings.">
        <SettingChoice
          title="Post suggestions"
          description="Lower-priority notes, such as tests, docs and naming, listed in a collapsed section of the review summary. Off posts only findings and questions."
          options={["true", "false"]}
          labels={{ true: "On", false: "Off" }}
          value={String(settings.reviewSuggestionsEnabled)}
          disabled={loading}
          onChange={(value) => onSave("review_suggestions_enabled", value)}
        />
      </SettingsGroup>
      <SettingsGroup title="Title filters" description="Skip low-value PRs by keyword.">
        <TokenEditor
          label="Ignored keywords"
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
