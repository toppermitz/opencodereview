import { useQuery, useQueryClient } from "@tanstack/react-query";
import { LockKeyhole } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { BrandLoading } from "../components/BrandLoading";
import { BrandMark } from "../components/BrandMark";
import { setupApi } from "../services/setupApi";
import { GitlabStep, PlanStep } from "./OnboardingAccess";
import { DoneStep } from "./OnboardingDone";
import { ctaPrimary, StepActions, StepDots, StepHeader } from "./OnboardingParts";
import { ReposStep } from "./OnboardingRepos";

type Step = 1 | 2 | 3 | 4;

/** First run: GitHub, an AI plan, repositories, then a first review — one centered step at a time. */
export function Onboarding() {
  const queryClient = useQueryClient();
  const [chosen, setChosen] = useState<Step | null>(null);
  const [savedCount, setSavedCount] = useState<number | null>(null);
  const status = useQuery({
    queryKey: ["setup", "status"],
    queryFn: () => setupApi.setupStatus(),
    retry: 1,
    retryDelay: 500,
    refetchInterval: (query) => (query.state.status === "error" ? 5000 : false),
  });
  // Resume where the server says, then stay put: connecting a first account must not skip the
  // rest of the step, only Continue and Back move on.
  useEffect(() => {
    if (chosen === null && status.data) setChosen(status.data.nextStep);
  }, [chosen, status.data]);
  const step: Step = chosen ?? status.data?.nextStep ?? 1;
  const refresh = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: ["setup", "status"] });
  }, [queryClient]);
  const offline = status.isError && !status.data;
  return (
    <div className="onboarding-shell flex min-h-dvh flex-col bg-canvas text-fg">
      <header className="flex items-center justify-between px-6 py-5">
        <div className="onboarding-brand flex items-center gap-2.5 text-body font-semibold [&_.brand-mark]:size-8">
          <BrandMark size="sm" />
          <span>OpenCodeReview</span>
        </div>
        <p className="flex items-center gap-1.5 text-caption text-fg-subtle max-sm:hidden">
          <LockKeyhole className="size-3.5" aria-hidden="true" />
          Sign-ins stay on this machine
        </p>
      </header>
      <main className="mx-auto flex w-full max-w-xl flex-1 flex-col gap-7 px-4 pt-[clamp(8px,3vh,32px)] pb-10">
        {status.isLoading ? (
          <BrandLoading label="Carregando seu workspace…" />
        ) : offline ? (
          <Offline retrying={status.isFetching} onRetry={refresh} />
        ) : (
          <>
            <StepDots step={step} total={4} />
            {step === 1 && (
              <GitlabStep
                connected={Boolean(status.data?.auth.loggedIn)}
                login={status.data?.auth.login ?? undefined}
                onChanged={refresh}
                onContinue={() => setChosen(2)}
              />
            )}
            {step === 2 && <PlanStep onBack={() => setChosen(1)} onContinue={() => setChosen(3)} />}
            {step === 3 && (
              <ReposStep
                onBack={() => setChosen(2)}
                onSaved={(count) => {
                  setSavedCount(count);
                  setChosen(4);
                }}
              />
            )}
            {step === 4 && (
              <DoneStep
                login={status.data?.auth.login ?? undefined}
                repoCount={savedCount ?? status.data?.selectedRepos.length ?? 0}
              />
            )}
          </>
        )}
      </main>
    </div>
  );
}

function Offline({ onRetry, retrying }: { onRetry: () => void; retrying: boolean }) {
  return (
    <>
      <StepHeader
        title="O OpenCodeReview não alcança o servidor"
        subtitle="A página carregou, mas a API do OpenCodeReview não respondeu. Ela roda no Docker ao lado desta página."
      />
      <div className="rounded-2xl border border-line bg-surface-muted/60 p-5 text-small text-fg">
        <p>
          Start OpenCodeReview from the repository folder, wait until the containers are healthy, then try again. This
          page reconnects by itself.
        </p>
        <code className="mt-3 block w-fit rounded-md bg-surface-sunken px-3 py-2 font-mono text-small">
          docker compose up -d
        </code>
      </div>
      <StepActions
        primary={
          <button type="button" className={ctaPrimary} disabled={retrying} onClick={onRetry}>
            {retrying ? "Verificando…" : "Tentar de novo"}
          </button>
        }
      />
    </>
  );
}
