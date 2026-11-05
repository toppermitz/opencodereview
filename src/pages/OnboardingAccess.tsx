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
        title="Connect GitLab"
        subtitle="OpenCodeReview reads your merge requests and posts its reviews as you."
      />
      <ChoiceList label="Code host">
        <ChoiceRow
          glyph={<GitLabMark className="size-5" />}
          name="GitLab"
          detail={
            connected ? (login ?? "Connected") : "Uses the GitLab CLI sign-in, kept in OpenCodeReview's Docker volume."
          }
          status={
            connected ? (
              <StatusPill tone="success">Connected</StatusPill>
            ) : !busy ? (
              <StatusPill tone="warning">Not connected</StatusPill>
            ) : undefined
          }
          action={
            !connected && !busy ? (
              <button type="button" className={rowAction} onClick={() => flow.start.mutate()}>
                {flow.failure ? "Try again" : "Connect"}
              </button>
            ) : undefined
          }
        >
          {!connected && (busy || flow.failure || flow.error) ? (
            <div className="grid gap-3">
              {busy &&
                (prompt.code ? (
                  <>
                    <p className="text-small font-medium text-fg">Enter this code on GitLab to approve access</p>
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
            Continue
          </button>
        }
        hint={connected ? undefined : "Connect GitLab to continue."}
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
    label: "Paste a token",
    hint: "From claude setup-token on another computer",
    icon: <KeyRound />,
    method: "code",
  },
  codex: {
    label: "Sign in with a code",
    hint: "Enter a one-time code on any device",
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
  if (provider.accounts > 0) return "Signed out";
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
  const hint = signIn ? "Finish signing in above." : ready ? "You can add more accounts later in Settings." : undefined;
  return (
    <>
      <StepHeader title="Connect your plan" subtitle="Reviews run on the AI plan you already pay for." />
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
        <ChoiceList label="Choose your AI provider">
          {list.map((provider) => {
            const open = signIn?.provider === provider.provider ? signIn : null;
            const connected = provider.ready > 0;
            const menu = provider.installed
              ? [
                  ...(connected
                    ? [
                        {
                          label: "Add another account",
                          icon: <Plus />,
                          hint: "Reviews switch to it when this one hits a limit",
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
                    <StatusPill tone="warning">Not installed</StatusPill>
                  ) : connected ? (
                    <StatusPill tone="success">Connected</StatusPill>
                  ) : undefined
                }
                action={
                  provider.installed && !connected && !open ? (
                    <button
                      type="button"
                      className={rowActionQuiet}
                      onClick={() => setSignIn({ provider: provider.provider, method: "oauth" })}
                    >
                      Connect
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
            Continue
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
