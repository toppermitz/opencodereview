import { Check } from "lucide-react";
import type { ReactNode } from "react";
import { cx, DSMenu, type DSMenuItem } from "../components/DesignSystem";

export const ctaPrimary =
  "inline-flex h-12 w-full cursor-pointer items-center justify-center gap-2 rounded-full bg-fg px-6 text-body font-semibold text-canvas transition hover:bg-white disabled:cursor-not-allowed disabled:opacity-40";
export const ctaSecondary =
  "inline-flex h-12 w-full cursor-pointer items-center justify-center gap-2 rounded-full bg-surface-muted px-6 text-body font-semibold text-fg transition hover:bg-surface-sunken disabled:cursor-not-allowed disabled:opacity-40";
export const rowAction =
  "inline-flex h-9 flex-none cursor-pointer items-center justify-center gap-1.5 rounded-lg bg-brand px-3.5 text-small font-semibold text-brand-fg transition hover:bg-brand-hover disabled:cursor-not-allowed disabled:opacity-50";
export const rowActionQuiet =
  "inline-flex h-9 flex-none cursor-pointer items-center justify-center gap-1.5 rounded-lg bg-surface-sunken px-3.5 text-small font-semibold text-fg transition hover:bg-line-strong disabled:cursor-not-allowed disabled:opacity-50";

/** One dot per step; the current one stretches into a pill. */
export function StepDots({ step, total }: { step: number; total: number }) {
  return (
    <ol className="flex items-center justify-center gap-1.5" aria-label={`Passo ${step} de ${total}`}>
      {Array.from({ length: total }, (_, index) => index + 1).map((dot) => (
        <li
          key={dot}
          aria-current={dot === step ? "step" : undefined}
          className={cx(
            "h-1.5 rounded-full transition-all",
            dot === step ? "w-6 bg-fg" : dot < step ? "w-1.5 bg-fg-muted" : "w-1.5 bg-line-strong",
          )}
        />
      ))}
    </ol>
  );
}

export function StepHeader({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <header className="flex flex-col items-center gap-3 text-center">
      <h1 className="m-0 text-[clamp(2rem,5vw,2.75rem)] font-semibold leading-[1.05] tracking-[-0.04em] text-fg">
        {title}
      </h1>
      <p className="max-w-md text-body text-fg-muted">{subtitle}</p>
    </header>
  );
}

/** The page's call to action: a full-width pill, an optional way back and a line saying what's missing. */
export function StepActions({
  primary,
  secondary,
  hint,
}: {
  primary: ReactNode;
  secondary?: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <footer className="mx-auto flex w-full max-w-sm flex-col items-center gap-2.5">
      {primary}
      {secondary}
      {hint && (
        <p className="min-h-5 text-center text-small text-fg-muted" role="status">
          {hint}
        </p>
      )}
    </footer>
  );
}

type Tone = "success" | "warning" | "neutral";
const pillTone: Record<Tone, string> = {
  success: "bg-success/15 text-success",
  warning: "bg-warning/15 text-warning",
  neutral: "bg-surface-sunken text-fg-muted",
};

export function StatusPill({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span
      className={cx(
        "inline-flex h-7 flex-none items-center gap-1 rounded-full px-2.5 text-caption font-semibold whitespace-nowrap",
        pillTone[tone],
      )}
    >
      {tone === "success" && <Check className="size-3.5 stroke-[2.5]" aria-hidden="true" />}
      {children}
    </span>
  );
}

/** A row of a grouped list: glyph, name with a detail line, status, action and a "more" menu; a sign-in unfolds below. */
export function ChoiceRow({
  glyph,
  name,
  detail,
  detailTone,
  status,
  action,
  menu,
  children,
}: {
  glyph: ReactNode;
  name: string;
  detail?: ReactNode;
  detailTone?: "warning";
  status?: ReactNode;
  action?: ReactNode;
  menu?: DSMenuItem[];
  children?: ReactNode;
}) {
  return (
    <li className="border-b border-line last:border-b-0" aria-label={name}>
      <div className="flex items-center gap-3.5 px-4 py-3.5 max-sm:flex-wrap">
        <span className="grid size-10 flex-none place-items-center rounded-xl bg-surface-sunken" aria-hidden="true">
          {glyph}
        </span>
        <div className="min-w-0 flex-1">
          <div className="truncate text-body font-semibold text-fg">{name}</div>
          {detail && (
            <div className={cx("truncate text-small", detailTone === "warning" ? "text-warning" : "text-fg-muted")}>
              {detail}
            </div>
          )}
        </div>
        <div className="flex flex-none items-center gap-2 max-sm:w-full max-sm:justify-end">
          {status}
          {action}
          {menu && menu.length > 0 && <DSMenu label={`More ${name} options`} size="iconSm" items={menu} />}
        </div>
      </div>
      {children && <div className="border-t border-line bg-surface/60 px-4 py-4 sm:pl-[4.25rem]">{children}</div>}
    </li>
  );
}

export function ChoiceList({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2.5">
      <h2 className="text-small font-semibold text-fg">{label}</h2>
      <ul className="overflow-hidden rounded-2xl border border-line bg-surface-muted/60">{children}</ul>
    </section>
  );
}
