import { Check, ChevronDown, X } from "lucide-react";
import { type ReactNode, useEffect, useId, useRef, useState } from "react";
import { BrandLoading } from "../../components/BrandLoading";
import { DSButton } from "../../components/DesignSystem";

type MultiSelectOption = { value: string; label: string; detail?: string; icon?: ReactNode };

export function MultiSelect({
  label,
  placeholder,
  options,
  selected,
  disabled,
  loading,
  presentation = "popover",
  onToggle,
}: {
  label: string;
  placeholder: string;
  options: MultiSelectOption[];
  selected: Set<string>;
  disabled?: boolean;
  loading?: boolean;
  presentation?: "popover" | "dialog";
  onToggle: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const id = useId();
  const ref = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    if (!open || disabled) return;
    if (presentation === "dialog") {
      const dialog = dialogRef.current;
      if (!dialog) return;
      const trigger = triggerRef.current;
      dialog.showModal();
      return () => {
        dialog.close();
        if (trigger?.isConnected) trigger.focus();
      };
    }
    const onPointerDown = (event: PointerEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open, disabled, presentation]);
  const selectedOptions = options.filter((option) => selected.has(option.value));
  const visibleOptions = options.filter((option) =>
    `${option.label} ${option.detail ?? ""}`.toLowerCase().includes(query.toLowerCase()),
  );
  const content = (
    <>
      <input
        className="settings-text-input h-9 w-full rounded-md border border-line bg-surface px-3 text-small text-fg outline-none focus:border-brand focus:ring-3 focus:ring-brand/15"
        aria-label={`Buscar ${label.toLowerCase()}`}
        value={query}
        placeholder="Buscar…"
        onChange={(event) => setQuery(event.target.value)}
        // biome-ignore lint/a11y/noAutofocus: the search box is only mounted when the user opens the picker, so focus moves into it on purpose
        autoFocus
      />
      <div className="settings-multiselect-options mt-2 flex max-h-72 flex-col gap-0.5 overflow-y-auto">
        {visibleOptions.length === 0 ? (
          loading ? (
            <BrandLoading size="compact" />
          ) : (
            <div className="settings-empty-inline px-3 py-4 text-center text-small text-fg-subtle">No matches</div>
          )
        ) : (
          visibleOptions.map((option) => (
            <button
              key={option.value}
              type="button"
              title={option.label}
              aria-pressed={selected.has(option.value)}
              className={`settings-multiselect-option flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2.5 py-1.5 text-left text-small transition-colors hover:bg-surface-muted ${selected.has(option.value) ? "selected text-brand" : "text-fg"}`}
              onClick={() => onToggle(option.value)}
            >
              <span className="settings-multiselect-option-content flex min-w-0 items-center gap-2">
                {option.icon && (
                  <span className="settings-multiselect-option-icon shrink-0" aria-hidden="true">
                    {option.icon}
                  </span>
                )}
                <span className="min-w-0">
                  <b>{option.label}</b>
                  {option.detail && <small>{option.detail}</small>}
                </span>
              </span>
              {selected.has(option.value) && <Check className="size-4" />}
            </button>
          ))
        )}
      </div>
    </>
  );
  return (
    <div ref={ref} className="settings-multiselect relative flex min-w-0 flex-col gap-1.5">
      <div className="settings-multiselect-label flex items-center justify-between text-caption font-medium text-fg-muted [&>span:last-child]:text-fg-subtle">
        <span id={`${id}-label`}>{label}</span>
        <span>
          {selectedOptions.length} selecionado{selectedOptions.length === 1 ? "" : "s"}
        </span>
      </div>
      <button
        ref={triggerRef}
        type="button"
        className="settings-multiselect-trigger flex min-h-9 w-full cursor-pointer items-center justify-between gap-2 rounded-lg border border-transparent bg-surface-muted px-3 py-1 text-left text-small text-fg transition-colors hover:bg-surface-sunken disabled:cursor-not-allowed disabled:opacity-50"
        aria-labelledby={`${id}-label`}
        aria-expanded={open && !disabled}
        aria-haspopup={presentation === "dialog" ? "dialog" : undefined}
        aria-controls={open && !disabled ? id : undefined}
        disabled={disabled}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="settings-multiselect-chips flex min-w-0 flex-wrap gap-1">
          {selectedOptions.length === 0 ? (
            <span className="settings-multiselect-placeholder text-fg-subtle">
              {loading ? "Carregando…" : placeholder}
            </span>
          ) : (
            selectedOptions.slice(0, 4).map((option) => (
              <span
                key={option.value}
                className="settings-chip-pill inline-flex h-6 max-w-40 items-center gap-1 truncate rounded-full bg-brand-soft px-2 text-caption font-medium text-brand [&>span]:cursor-pointer"
                title={option.label}
              >
                {option.label}
                {/* biome-ignore lint/a11y/noStaticElementInteractions: mouse shortcut inside the trigger button (a nested button is invalid); keyboard users deselect in the list */}
                {/* biome-ignore lint/a11y/useKeyWithClickEvents: same as above, the option list is the keyboard path */}
                <span
                  onClick={(event) => {
                    event.stopPropagation();
                    onToggle(option.value);
                  }}
                >
                  <X className="size-3" />
                </span>
              </span>
            ))
          )}
          {selectedOptions.length > 4 && (
            <span
              className="settings-chip-pill inline-flex h-6 max-w-40 items-center gap-1 truncate rounded-full bg-brand-soft px-2 text-caption font-medium text-brand [&>span]:cursor-pointer"
              title={selectedOptions
                .slice(4)
                .map((option) => option.label)
                .join("\n")}
            >
              +{selectedOptions.length - 4}
            </span>
          )}
        </span>
        <ChevronDown className={`size-4 transition ${open ? "rotate-180" : ""}`} />
      </button>
      {open &&
        !disabled &&
        (presentation === "dialog" ? (
          <dialog
            ref={dialogRef}
            id={id}
            className="settings-multiselect-dialog m-auto w-full max-w-lg rounded-2xl border border-line bg-surface p-0 text-fg shadow-pop backdrop:bg-slate-900/40"
            aria-labelledby={`${id}-title`}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setOpen(false);
              }
            }}
            onCancel={(event) => {
              event.preventDefault();
              setOpen(false);
            }}
            onClick={(event) => {
              if (event.target !== event.currentTarget) return;
              const bounds = event.currentTarget.getBoundingClientRect();
              if (
                event.clientX < bounds.left ||
                event.clientX > bounds.right ||
                event.clientY < bounds.top ||
                event.clientY > bounds.bottom
              )
                setOpen(false);
            }}
          >
            <header className="flex items-center justify-between gap-3 border-b border-line px-5 py-3">
              <h2 id={`${id}-title`} className="text-heading text-fg">
                {label}
              </h2>
              <DSButton tone="ghost" aria-label={`Close ${label.toLowerCase()}`} onClick={() => setOpen(false)}>
                <X className="size-4" />
              </DSButton>
            </header>
            <div className="max-h-[60vh] overflow-y-auto px-5 py-4">{content}</div>
            <footer className="flex items-center justify-between gap-2 border-t border-line bg-surface-muted/60 px-5 py-3 text-small text-fg-muted">
              <span>
                {selectedOptions.length} selecionado{selectedOptions.length === 1 ? "" : "s"}
              </span>
              <DSButton tone="primary" onClick={() => setOpen(false)}>
                Done
              </DSButton>
            </footer>
          </dialog>
        ) : (
          <div
            id={id}
            className="settings-multiselect-popover absolute top-full right-0 left-0 z-40 mt-1 rounded-xl border border-line bg-surface p-2 shadow-pop"
          >
            {content}
          </div>
        ))}
    </div>
  );
}
