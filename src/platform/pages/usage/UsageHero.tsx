import { Hint, Tip } from "../../../components/Tip";
import type { UsageReport } from "../../../types";
import { ProviderGlyph } from "../../components/AccountsPanel";
import { attributedRows, compactTokens, isProvider, pct } from "./format";

const plural = (count: number, one: string, many = `${one}s`) =>
  `${count.toLocaleString()} ${count === 1 ? one : many}`;

/** One part of the token total: label, amount, share and a bar of that share. */
function MixRow({
  label,
  tokens,
  total,
  bar,
  hint,
}: {
  label: string;
  tokens: number;
  total: number;
  bar: string;
  hint: string;
}) {
  return (
    <li className="grid grid-cols-[7rem_minmax(0,1fr)_4.5rem_2.75rem] items-center gap-3 text-small">
      <span className="inline-flex items-center gap-1.5 text-fg-muted">
        {label}
        <Hint text={hint} />
      </span>
      <span className="h-1.5 overflow-hidden rounded-full bg-surface-sunken">
        <span className={`block h-full rounded-full ${bar}`} style={{ width: pct(tokens, total) }} />
      </span>
      <span className="text-right font-mono text-fg tabular-nums">{compactTokens(tokens)}</span>
      <span className="text-right font-mono text-caption text-fg-subtle tabular-nums">{pct(tokens, total)}</span>
    </li>
  );
}

export function UsageHero({ report }: { report: UsageReport }) {
  const { totals } = report;
  const providers = attributedRows(report.byProvider);
  return (
    <div className="flex h-full min-w-0 flex-col gap-6">
      <div className="flex flex-col gap-2">
        <Tip content={`${totals.tokens.toLocaleString()} tokens`} className="w-fit">
          <p className="font-sans text-hero text-fg tabular-nums" data-usage-hero>
            {compactTokens(totals.tokens)}
          </p>
        </Tip>
        <p className="flex flex-wrap items-center gap-x-1.5 text-small text-fg-muted">
          <span>
            tokens · {plural(totals.runs, "execução", "execuções")} em {plural(totals.prs, "MR")}
          </span>
          {totals.runsWithoutUsage > 0 && (
            <span className="inline-flex items-center gap-1 text-fg-subtle">
              · {plural(totals.runsWithoutUsage, "execução", "execuções")} sem uso reportado
              <Hint text="Essas execuções terminaram sem o CLI de IA reportar tokens, então não entram nos totais." />
            </span>
          )}
        </p>
      </div>
      {totals.tokens > 0 && (
        <ul className="flex flex-col gap-2.5" aria-label="O que os tokens são">
          <MixRow
            label="Entrada"
            tokens={totals.inputTokens}
            total={totals.tokens}
            bar="bg-brand"
            hint="Tokens de prompt que o modelo processou: o diff, o contexto e as instruções."
          />
          <MixRow
            label="Saída"
            tokens={totals.outputTokens}
            total={totals.tokens}
            bar="bg-brand/70"
            hint="Tokens que o modelo escreveu: achados, perguntas e o raciocínio."
          />
          <MixRow
            label="Escritas de cache"
            tokens={totals.cacheCreationTokens}
            total={totals.tokens}
            bar="bg-brand/40"
            hint="Tokens de prompt gravados no cache do provedor para chamadas seguintes reutilizarem."
          />
        </ul>
      )}
      <div className="mt-auto flex flex-col gap-2 border-t border-line pt-3">
        {totals.cacheReadTokens > 0 && (
          <p className="flex items-center justify-between gap-3 text-small">
            <span className="inline-flex items-center gap-1.5 text-fg-muted">
              Lido do cache
              <Hint text="Contexto que o provedor releu do cache de prompt em vez de processar de novo. Não entra nos tokens." />
            </span>
            <span className="font-mono text-fg-muted tabular-nums">+ {compactTokens(totals.cacheReadTokens)}</span>
          </p>
        )}
        {providers.length >= 2 && (
          <ul className="flex flex-wrap gap-x-4 gap-y-1 text-caption text-fg-muted" aria-label="Tokens por provedor">
            {providers.map((row) => (
              <li key={row.key} className="inline-flex items-center gap-1.5 tabular-nums">
                {isProvider(row.provider) && <ProviderGlyph provider={row.provider} />}
                <span>{row.label}</span>
                <span className="text-fg-subtle">{pct(row.tokens, totals.tokens)}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
