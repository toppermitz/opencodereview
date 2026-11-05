import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BrandLoading, BrandLoadingIcon } from "./BrandLoading";

const render = (markup: string) => new DOMParser().parseFromString(markup, "text/html");
const svgFrom = (url: string) =>
  new DOMParser().parseFromString(decodeURIComponent(url.split(",")[1]), "image/svg+xml");

describe("BrandLoading", () => {
  it("animates the mascot inline without changing button text, with a still copy for reduced motion", () => {
    const document = render(
      renderToStaticMarkup(
        <button type="button">
          <BrandLoadingIcon size={24} />
          Starting…
        </button>,
      ),
    );
    const image = document.querySelector(".brand-loading-icon")!;
    expect(document.querySelector("button")?.textContent).toBe("Starting…");
    expect(image.getAttribute("aria-hidden")).toBe("true");
    expect(image.getAttribute("alt")).toBe("");
    expect(image.getAttribute("width")).toBe("24");
    const svg = svgFrom(image.getAttribute("src")!);
    expect(svg.querySelector("parsererror")).toBeNull();
    expect(svg.documentElement.getAttribute("data-motion")).toBe("loading");
    expect(svg.querySelector("style")?.textContent).toContain("prefers-reduced-motion: no-preference");
    expect(svg.querySelector("style")?.textContent).toContain("animation: ocr-mascot-icon-scan");
    const still = svgFrom(
      document.querySelector('source[media="(prefers-reduced-motion: reduce)"]')!.getAttribute("srcset")!,
    );
    expect(still.documentElement.getAttribute("data-motion")).toBe("none");
  });

  it("announces loading next to decorative inline artwork", () => {
    const document = render(renderToStaticMarkup(<BrandLoading />));
    const status = document.querySelector('[role="status"]')!;
    expect(status.getAttribute("aria-live")).toBe("polite");
    expect(status.getAttribute("aria-atomic")).toBe("true");
    expect(status.querySelector("p")?.textContent).toBe("Carregando…");
    const svg = status.querySelector('.brand-loading-art[aria-hidden="true"] svg[data-motion="loading"]')!;
    expect(svg.querySelector("title")?.textContent).toBe("OpenCodeReview mascot");
    expect(document.querySelector("img, image, script, foreignObject, .animate-pulse")).toBeNull();
    expect(status.querySelector("style")?.textContent).toContain(`animation: ${svg.id}-scan`);
  });

  it("supports a compact size", () => {
    const document = render(renderToStaticMarkup(<BrandLoading size="compact" label="Loading repositories…" />));
    expect(document.querySelector('.brand-loading[data-size="compact"][role="status"] p')?.textContent).toBe(
      "Loading repositories…",
    );
  });

  it("isolates ids, classes and keyframes between copies", () => {
    const document = render(
      renderToStaticMarkup(
        <>
          <BrandLoading />
          <BrandLoading label="Again" />
        </>,
      ),
    );
    const ids = [...document.querySelectorAll("[id]")].map((element) => element.id);
    expect(new Set(ids).size).toBe(ids.length);
    const [first, second] = [...document.querySelectorAll(".brand-loading-art > svg")];
    expect(first.getAttribute("class")).not.toBe(second.getAttribute("class"));
    expect(document.body.innerHTML).not.toContain("ocr-mascot-scan");
  });
});
