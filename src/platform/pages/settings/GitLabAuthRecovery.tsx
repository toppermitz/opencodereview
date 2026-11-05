import { RefreshCw } from "lucide-react";
import { DSButton } from "../../../components/DesignSystem";

export function GitLabAuthRecovery({
  prompt,
  doneMessage,
  doneSuccess,
  loading,
  onConnect,
  onRefresh,
}: {
  prompt: { code?: string; url?: string };
  doneMessage?: string;
  doneSuccess?: boolean;
  loading: boolean;
  onConnect: () => void;
  onRefresh: () => void;
}) {
  return (
    <div className="grid gap-3 rounded-lg border border-line bg-[color-mix(in_srgb,var(--surface-2)_70%,transparent)] p-4">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="text-sm font-semibold">Autorização do GitLab</div>
          <div className="mt-1 text-sm text-fg-muted">
            Digite o código de dispositivo no GitLab para renovar o acesso. Prefira o link de verificação quando
            aparecer.
          </div>
        </div>
        <DSButton onClick={onRefresh} disabled={loading}>
          <RefreshCw className="size-3.5" />
          Re-check
        </DSButton>
      </div>
      {prompt.url && (
        <a
          className="text-sm font-semibold text-brand underline underline-offset-4"
          href={prompt.url}
          target="_blank"
          rel="noreferrer"
        >
          Abrir página de verificação do GitLab
        </a>
      )}
      {prompt.code && (
        <code className="w-fit rounded-lg bg-surface px-3 py-2 text-sm font-semibold text-brand">{prompt.code}</code>
      )}
      {doneMessage && (
        <div className={`text-sm font-semibold ${doneSuccess ? "text-success" : "text-danger"}`}>{doneMessage}</div>
      )}
      <div className="flex justify-end gap-3">
        <DSButton onClick={onConnect} disabled={loading} tone="primary">
          {loading ? "Iniciando" : "Iniciar autorização"}
        </DSButton>
      </div>
    </div>
  );
}
