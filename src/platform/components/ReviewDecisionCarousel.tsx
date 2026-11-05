export interface ReviewDecisionCarouselOption {
  value: string;
  label: string;
  description: string;
  tone?: "danger" | "primary" | "secondary";
}

interface ReviewDecisionCarouselProps {
  options: ReadonlyArray<ReviewDecisionCarouselOption>;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
}

export function ReviewDecisionCarousel({
  options,
  value,
  onChange,
  disabled = false,
}: Readonly<ReviewDecisionCarouselProps>) {
  return (
    <div
      className="review-decision-carousel flex min-w-0 gap-2 overflow-x-auto pb-1 [scrollbar-width:thin]"
      role="radiogroup"
      aria-label="Decisões da revisão"
    >
      {options.map((option) => {
        const selected = option.value === value;
        return (
          // biome-ignore lint/a11y/useSemanticElements: custom-styled radio card; a native input cannot hold this card layout
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            className={`review-decision-option flex w-44 flex-none cursor-pointer flex-col gap-0.5 rounded-lg border px-3 py-2 text-left transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${selected ? (option.tone === "danger" ? "is-selected border-rose-400 bg-rose-50 ring-1 ring-rose-300" : "is-selected border-brand/50 bg-brand-soft ring-1 ring-brand/30") : "border-line bg-surface hover:border-line-strong hover:bg-surface-muted"}`}
            data-tone={option.tone ?? "secondary"}
            disabled={disabled}
            onClick={() => onChange(option.value)}
          >
            <span
              className={`review-decision-option-label text-small font-semibold ${option.tone === "danger" ? "text-rose-700" : "text-fg"}`}
            >
              {option.label}
            </span>
            <span className="review-decision-option-description text-caption text-fg-subtle">{option.description}</span>
          </button>
        );
      })}
    </div>
  );
}
