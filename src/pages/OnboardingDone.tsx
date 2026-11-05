import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { ArrowRight, Check, GitPullRequest } from "lucide-react";
import { useEffect } from "react";
import { friendlyError } from "../components/AccessConnect";
import { BrandLoadingIcon } from "../components/BrandLoading";
import { setupApi } from "../services/setupApi";
import type { ReviewCard } from "../types";
import { PROVIDERS_QUERY_KEY } from "./OnboardingAccess";
import {
  ChoiceList,
  ChoiceRow,
  ctaPrimary,
  rowAction,
  rowActionQuiet,
  StatusPill,
  StepActions,
  StepHeader,
} from "./OnboardingParts";

const running = new Set(["queued", "reviewing"]);
const reviewed = new Set(["ready", "reviewed_cleanly"]);

function Connected({ name, detail }: { name: string; detail: string }) {
  return (
    <li className="flex min-w-0 items-center gap-2.5 px-4 py-3">
      <span
        className="grid size-6 flex-none place-items-center rounded-full bg-success/15 text-success"
        aria-hidden="true"
      >
        <Check className="size-3.5 stroke-[2.5]" />
      </span>
      <span className="min-w-0">
        <span className="block text-small font-semibold text-fg">{name}</span>
        <span className="block truncate text-caption text-fg-muted">{detail}</span>
      </span>
    </li>
  );
}

/** Confirms what is connected, then offers an open merge request that still needs its first review. */
export function DoneStep({ login, repoCount }: { login?: string; repoCount: number }) {
  const navigate = useNavigate();
  const providers = useQuery({ queryKey: PROVIDERS_QUERY_KEY, queryFn: () => setupApi.aiProviders() });
  const poll = useMutation({ mutationFn: () => setupApi.pollReviews() });
  const { mutate: startPoll } = poll;
  useEffect(() => {
    startPoll();
  }, [startPoll]);
  const board = useQuery({
    queryKey: ["setup", "first-review-board"],
    queryFn: () => setupApi.listReviewBoard(),
    refetchInterval: 4000,
  });
  const review = useMutation({
    mutationFn: (pr: ReviewCard) => setupApi.startReview(pr.id),
    onSuccess: () => void navigate({ to: "/platform/reviews" }),
  });
  const plans = (providers.data ?? [])
    .filter((provider) => provider.ready > 0)
    .map((provider) =>
      provider.label && provider.label !== provider.name ? `${provider.name} · ${provider.label}` : provider.name,
    );
  const open = (board.data ?? []).flatMap((group) => group.items).filter((pr) => pr.state === "open");
  const pending = open
    .filter((pr) => !reviewed.has(pr.lane))
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, 3);
  const finding = board.isLoading || (poll.isPending && open.length === 0);
  return (
    <>
      <StepHeader
        title="Tudo pronto"
        subtitle="Seu código está sendo indexado em segundo plano. Revise um merge request aberto agora."
      />
      <ul
        className="grid overflow-hidden rounded-2xl border border-line bg-surface-muted/60 sm:grid-cols-3 sm:divide-x max-sm:divide-y divide-line"
        aria-label="Conectado"
      >
        <Connected name="GitLab" detail={login ?? "Conectado"} />
        <Connected name="Plano de IA" detail={plans.join(", ") || "Conectado"} />
        <Connected name="Repositórios" detail={`${repoCount} selecionados`} />
      </ul>
      <ChoiceList label="Sua primeira revisão">
        {finding ? (
          <li className="flex items-center gap-2 px-4 py-4 text-small text-fg-muted">
            <BrandLoadingIcon />
            Looking for open merge requests…
          </li>
        ) : pending.length === 0 ? (
          <li className="px-4 py-4 text-small text-fg-muted">
            {open.length
              ? "Todo merge request aberto já foi revisado. Novos são revisados assim que abertos."
              : "Ainda não há merge requests abertos. Novos são revisados assim que abertos."}
          </li>
        ) : (
          pending.map((pr) => (
            <ChoiceRow
              key={pr.id}
              glyph={<GitPullRequest className="size-4.5 text-success" />}
              name={pr.title}
              detail={`${pr.repoFullName} #${pr.number}`}
              status={running.has(pr.lane) ? <StatusPill tone="neutral">Revisando…</StatusPill> : undefined}
              action={
                running.has(pr.lane) ? (
                  <Link to="/platform/reviews" className={rowActionQuiet}>
                    Open
                  </Link>
                ) : (
                  <button
                    type="button"
                    className={rowAction}
                    disabled={review.isPending}
                    onClick={() => review.mutate(pr)}
                  >
                    {review.isPending && review.variables?.id === pr.id ? "Iniciando…" : "Revisar"}
                  </button>
                )
              }
            />
          ))
        )}
      </ChoiceList>
      {(review.error || board.error) && (
        <p className="text-center text-small font-semibold text-danger" role="alert">
          {friendlyError(review.error ?? board.error)}
        </p>
      )}
      <StepActions
        primary={
          <Link to="/platform/reviews" className={ctaPrimary}>
            Go to reviews
            <ArrowRight className="size-4" aria-hidden="true" />
          </Link>
        }
        hint="Altere contas e repositórios a qualquer momento em Configurações."
      />
    </>
  );
}
