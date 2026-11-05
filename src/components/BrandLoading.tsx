import { useId, useMemo } from "react";
import mascotSvg from "../../assets/mascot.svg?raw";

/** The mascot reading side to side; each copy gets its own ids, class and keyframes. */
function loadingArtwork(prefix: string, motion: "loading" | "none" = "loading") {
  return mascotSvg.replaceAll("ocr-mascot", prefix).replace('data-motion="none"', `data-motion="${motion}"`);
}

const iconUrl = `data:image/svg+xml,${encodeURIComponent(loadingArtwork("ocr-mascot-icon"))}`;
const stillIconUrl = `data:image/svg+xml,${encodeURIComponent(loadingArtwork("ocr-mascot-icon", "none"))}`;

export function BrandLoadingIcon({ size = 20 }: { size?: number }) {
  return (
    <picture className="brand-loading-picture">
      <source media="(prefers-reduced-motion: reduce)" srcSet={stillIconUrl} />
      <img
        className="brand-loading-icon"
        src={iconUrl}
        width={size}
        height={size}
        alt=""
        aria-hidden="true"
        draggable={false}
      />
    </picture>
  );
}

export function BrandLoading({
  label = "Carregando…",
  size = "default",
}: {
  label?: string;
  size?: "default" | "compact";
}) {
  const id = useId();
  const artwork = useMemo(() => loadingArtwork(`ocr-mascot-${id.replace(/[^a-zA-Z0-9_-]/g, "")}`), [id]);

  return (
    <div className="brand-loading" data-size={size} role="status" aria-live="polite" aria-atomic="true">
      {/* biome-ignore lint/security/noDangerouslySetInnerHtml: bundled mascot.svg asset with only its ids rewritten, no user input; inline so its CSS animation runs */}
      <div className="brand-loading-art" aria-hidden="true" dangerouslySetInnerHTML={{ __html: artwork }} />
      <p>{label}</p>
    </div>
  );
}
