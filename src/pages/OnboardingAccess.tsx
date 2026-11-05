import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Plus, Smartphone } from "lucide-react";
import { useCallback, useState } from "react";
import {
  ClaudeSignIn,
  ClaudeTokenSignIn,
  CodexSignIn,
  CopyCode,
  friendlyError,
  useGitlabFlow,
} from "../components/AccessConnect";
import { BrandLoadingIcon } from "../components/BrandLoading";
import { GitLabMark } from "../components/GitLabMark";
import { ProviderGlyph } from "../platform/components/AccountsPanel";
import { setupApi } from "../services/setupApi";
import type { AiProvider } from "../types";
import {
  ChoiceList,
  ChoiceRow,
  ctaPrimary,
  ctaSecondary,
  rowAction,
  rowActionQuiet,
  StatusPill,
  StepActions,
  StepHeader,
} from "./OnboardingParts";

export const PROVIDERS_QUERY_KEY = ["setup", "providers"] as const;

export function GitlabStep({
  connected,
  login,
  onChanged,
  onContinue,
}: {
  connected: boolean;
  login?: string;
  onChanged: () => void;
  onContinue: () => void;
}) {
  const flow = useGitlabFlow(connected, onChanged);
  const { prompt } = flow;
  const busy = flow.running || flow.start.isPending;
  return (
    <>
      <StepHeader
        title="Conectar GitLab"
        subtitle="OpenCodeReview reads your merge requests and posts its reviews as you."
      />
      <ChoiceList label="Hospedeiro de código">
        <ChoiceRow
          glyph={<GitLabMark className="size-5" />}
          name="GitLab"
          detail={
            connected
              ? (login ?? "Conectado")
              : "Autorize com um código de dispositivo GitLab; o token fica no volume Docker do OpenCodeReview."
          }
          status={
            connected ? (
              <StatusPill tone="success">Conectado</StatusPill>
            ) : !busy ? (
              <StatusPill tone="warning">Não conectado</StatusPill>
            ) : undefined
          }
          action={
            !connected && !busy ? (
              <button type="button" className={rowAction} onClick={() => flow.start.mutate()}>
                {flow.failure ? "Tentar de novo" : "Conectar"}
              </button>
            ) : undefined
          }
        >
          {!connected && (busy || flow.failure || flow.error) ? (
            <div className="grid gap-3">
              {busy &&
                (prompt.code ? (
                  <>
                    <p className="text-small font-medium text-fg">Enter this device code on GitLab to approve access</p>
                    <CopyCode code={prompt.code} />
                    <div className="flex flex-wrap items-center gap-3">
                      <p className="flex items-center gap-2 text-small text-fg-muted">
                        <BrandLoadingIcon />
                        <span>Waiting for approval. This page updates by itself.</span>
                      </p>
                      {prompt.url && (
                        <a
                          className={rowAction}
                          href={prompt.url}
                          target="_blank"
                          rel="noreferrer"
                          onClick={() => void navigator.clipboard?.writeText(prompt.code ?? "")}
                        >
                          Open GitLab
                        </a>
                      )}
                    </div>
                  </>
                ) : (
                  <p className="flex items-center gap-2 text-small text-fg-muted">
                    <BrandLoadingIcon />
                    <span>Starting GitLab sign-in…</span>
                  </p>
                ))}
              {!busy && flow.failure && (
                <p className="text-small font-semibold text-danger" role="alert">
                  {flow.failure}
                </p>
              )}
              {flow.error && (
                <p className="text-small font-semibold text-danger" role="alert">
                  {friendlyError(flow.error)}
                </p>
              )}
            </div>
          ) : null}
        </ChoiceRow>
      </ChoiceList>
      <StepActions
        primary={
          <button type="button" className={ctaPrimary} disabled={!connected} onClick={onContinue}>
            Continuar
          </button>
        }
        hint={connected ? undefined : "Conecte o GitLab para continuar."}
      />
    </>
  );
}

type SignIn = { provider: AiProvider["provider"]; method: "oauth" | "code" };

const planDetail: Record<AiProvider["provider"], string> = {
  claude: "Pro, Max or Team plan",
  codex: "ChatGPT Plus or Pro plan",
};

/** The second way into each provider, offered from the row's menu. */
const alternative: Record<
  AiProvider["provider"],
  { label: string; hint: string; icon: React.ReactNode; method: SignIn["method"] }
> = {
  claude: {
    label: "Colar um token",
    hint: "De claude setup-token em outro computador",
    icon: <KeyRound />,
    method: "code",
  },
  codex: {
    label: "Entrar com um código",
    hint: "Digite um código de uso único em qualquer dispositivo",
    icon: <Smartphone />,
    method: "code",
  },
};

function providerDetail(provider: AiProvider) {
  if (!provider.installed) return `The ${provider.name} CLI is missing from this OpenCodeReview install.`;
  if (provider.ready > 0)
    return [
      provider.label && provider.label !== provider.name ? provider.label : planDetail[provider.provider],
      provider.ready > 1 ? `+${provider.ready - 1} more` : "",
    ]
      .filter(Boolean)
      .join(" · ");
  if (provider.accounts > 0) return "Desconectado";
  return planDetail[provider.provider];
}

export function PlanStep({ onBack, onContinue }: { onBack: () => void; onContinue: () => void }) {
  const queryClient = useQueryClient();
  const [signIn, setSignIn] = useState<SignIn | null>(null);
  const providers = useQuery({ queryKey: PROVIDERS_QUERY_KEY, queryFn: () => setupApi.aiProviders() });
  const refresh = useCallback(() => {
    for (const key of [PROVIDERS_QUERY_KEY, ["setup", "claude-auth"], ["setup", "status"], ["accounts"]])
      void queryClient.invalidateQueries({ queryKey: key });
  }, [queryClient]);
  const finish = useCallback(() => {
    setSignIn(null);
    refresh();
  }, [refresh]);
  const list = providers.data ?? [];
  const ready = list.some((provider) => provider.ready > 0);
  const hint = signIn
    ? "Conclua o login acima."
    : ready
      ? "Você pode adicionar mais contas depois em Configurações."
      : undefined;
  return (
    <>
      <StepHeader title="Conecte seu plano" subtitle="As revisões usam o plano de IA que você já paga." />
      {providers.isLoading ? (
        <p className="flex items-center justify-center gap-2 text-small text-fg-muted">
          <BrandLoadingIcon />
          Checking the AI providers on this machine…
        </p>
      ) : providers.error ? (
        <p className="text-center text-small font-semibold text-danger" role="alert">
          {friendlyError(providers.error)}
        </p>
      ) : (
        <ChoiceList label="Escolha seu provedor de IA">
          {list.map((provider) => {
            const open = signIn?.provider === provider.provider ? signIn : null;
            const connected = provider.ready > 0;
            const menu = provider.installed
              ? [
                  ...(connected
                    ? [
                        {
                          label: "Adicionar outra conta",
                          icon: <Plus />,
                          hint: "As revisões passam para ela quando esta atingir o limite",
                          onSelect: () => setSignIn({ provider: provider.provider, method: "oauth" }),
                        },
                      ]
                    : []),
                  ...(alternative[provider.provider].method === "oauth"
                    ? []
                    : [
                        {
                          ...alternative[provider.provider],
                          onSelect: () =>
                            setSignIn({ provider: provider.provider, method: alternative[provider.provider].method }),
                        },
                      ]),
                ]
              : [];
            return (
              <ChoiceRow
                key={provider.provider}
                glyph={
                  <span className="scale-125">
                    <ProviderGlyph provider={provider.provider} />
                  </span>
                }
                name={provider.name}
                detail={providerDetail(provider)}
                detailTone={provider.installed ? undefined : "warning"}
                status={
                  !provider.installed ? (
                    <StatusPill tone="warning">Não instalado</StatusPill>
                  ) : connected ? (
                    <StatusPill tone="success">Conectado</StatusPill>
                  ) : undefined
                }
                action={
                  provider.installed && !connected && !open ? (
                    <button
                      type="button"
                      className={rowActionQuiet}
                      onClick={() => setSignIn({ provider: provider.provider, method: "oauth" })}
                    >
                      Conectar
                    </button>
                  ) : undefined
                }
                menu={open ? undefined : menu}
              >
                {open &&
                  (provider.provider === "codex" ? (
                    <CodexSignIn
                      mode={open.method === "code" ? "device" : "browser"}
                      onDone={finish}
                      onCancel={() => setSignIn(null)}
                    />
                  ) : open.method === "code" ? (
                    <ClaudeTokenSignIn onDone={finish} onCancel={() => setSignIn(null)} />
                  ) : (
                    <ClaudeSignIn onDone={finish} onCancel={() => setSignIn(null)} />
                  ))}
              </ChoiceRow>
            );
          })}
        </ChoiceList>
      )}
      <StepActions
        primary={
          <button type="button" className={ctaPrimary} disabled={!ready} onClick={onContinue}>
            Continuar
          </button>
        }
        secondary={
          <button type="button" className={ctaSecondary} onClick={onBack}>
            Back
          </button>
        }
        hint={hint}
      />
    </>
  );
}
