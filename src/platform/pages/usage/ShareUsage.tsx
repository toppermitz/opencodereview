import { Check, Copy, Download, Share2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { BrandLoading } from "../../../components/BrandLoading";
import { DSButton, DSError } from "../../../components/DesignSystem";
import type { UsageReport } from "../../../types";
import { formatError } from "../../lib/utils";
import { renderShareCard, SHARE_HEIGHT, SHARE_WIDTH, shareCardData } from "./shareCard";

const canCopyImage = () => typeof ClipboardItem !== "undefined" && Boolean(navigator.clipboard?.write);

function ShareDialog({ report, onClose }: { report: UsageReport; onClose: () => void }) {
  // The numbers as the dialog opened: the report refetches while a run is in progress, and the
  // image being shown, copied or downloaded must not change or lose its URL under the viewer.
  const [data] = useState(() => shareCardData(report));
  const [image, setImage] = useState<{ blob: Blob; url: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    renderShareCard(document.createElement("canvas"), data)
      .then((blob) => {
        if (cancelled) return;
        url = URL.createObjectURL(blob);
        setImage({ blob, url });
      })
      .catch((cause) => !cancelled && setError(formatError(cause)));
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [data]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const copy = async () => {
    if (!image) return;
    try {
      await navigator.clipboard.write([new ClipboardItem({ "image/png": image.blob })]);
      setCopied(true);
    } catch (cause) {
      setError(formatError(cause));
    }
  };

  const download = () => {
    if (!image) return;
    const link = document.createElement("a");
    link.href = image.url;
    link.download = `opencodereview-usage-${data.days}d.png`;
    link.click();
  };

  return (
    <div className="fixed inset-0 z-50 grid place-items-center p-4">
      <button
        type="button"
        tabIndex={-1}
        aria-label="Fechar"
        className="absolute inset-0 cursor-default bg-black/60"
        onClick={onClose}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Compartilhar imagem de uso"
        className="relative flex w-[min(40rem,100%)] flex-col gap-4 rounded-2xl border border-line-strong bg-surface p-5 shadow-pop"
      >
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 className="text-heading text-fg">Compartilhe seu uso</h2>
            <p className="text-small text-fg-subtle">
              Só totais: sem nome de repositório, merge request, conta ou modelo.
            </p>
          </div>
          <DSButton tone="ghost" size="iconSm" aria-label="Fechar" onClick={onClose}>
            <X className="size-4" aria-hidden="true" />
          </DSButton>
        </div>
        <div
          className="grid w-full place-items-center overflow-hidden rounded-xl border border-line bg-canvas"
          style={{ aspectRatio: `${SHARE_WIDTH} / ${SHARE_HEIGHT}` }}
        >
          {image ? (
            <img src={image.url} alt="Resumo de uso para compartilhar" className="h-full w-full" />
          ) : error ? null : (
            <BrandLoading size="compact" label="Desenhando imagem…" />
          )}
        </div>
        {error && <DSError>{error}</DSError>}
        <div className="flex justify-end gap-2">
          <DSButton disabled={!image} onClick={download}>
            <Download className="size-4" aria-hidden="true" />
            Download PNG
          </DSButton>
          {canCopyImage() && (
            <DSButton tone="primary" disabled={!image} onClick={() => void copy()}>
              {copied ? (
                <Check className="size-4" aria-hidden="true" />
              ) : (
                <Copy className="size-4" aria-hidden="true" />
              )}
              {copied ? "Copiado" : "Copiar imagem"}
            </DSButton>
          )}
        </div>
      </div>
    </div>
  );
}

/** Opens a shareable image of the period's totals. */
export function ShareUsage({ report }: { report: UsageReport }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <DSButton tone="ghost" size="sm" onClick={() => setOpen(true)}>
        <Share2 className="size-3.5" aria-hidden="true" />
        Share
      </DSButton>
      {open && <ShareDialog report={report} onClose={() => setOpen(false)} />}
    </>
  );
}
