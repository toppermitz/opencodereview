import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import mascotUrl from "../../../assets/mascot.svg";
import { PlatformLayout } from "./PlatformLayout";

const location = vi.hoisted(() => ({ pathname: "/platform/reviews" }));
vi.mock("@tanstack/react-router", () => ({
  useLocation: ({ select }: { select: (value: typeof location) => unknown }) => select(location),
  Link: ({ children, to }: { children: ReactNode; to?: string }) => <a href={to}>{children}</a>,
  Outlet: () => <div>Workspace content</div>,
}));
vi.mock("../../services/setupApi", () => ({
  setupApi: {
    listReviewBoard: async () => [
      {
        lane: "manual_required",
        title: "Precisa de você",
        items: [
          { id: 1, lane: "manual_required" },
          { id: 2, lane: "ready" },
        ],
      },
    ],
  },
}));

it("uses the same attention count as the review workspace", () => {
  const client = new QueryClient();
  client.setQueryData(
    ["reviews", "board"],
    [
      {
        lane: "manual_required",
        title: "Precisa de você",
        items: [
          { id: 1, lane: "manual_required" },
          { id: 2, lane: "ready" },
        ],
      },
    ],
  );
  try {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <PlatformLayout />
      </QueryClientProvider>,
    );
    expect(html).toContain('title="Revisões que precisam da sua atenção">2</span>');
    const mark = new DOMParser().parseFromString(html, "text/html").querySelector("[data-topbar] .brand-mark.sm");
    expect(mark).not.toBeNull();
    expect(mark?.getAttribute("aria-hidden")).toBe("true");
    expect(mark?.querySelector("img")?.getAttribute("src")).toBe(mascotUrl);
    expect(mark?.querySelector("img")?.getAttribute("width")).toBe("36");
    expect(mark?.querySelector("img")?.getAttribute("height")).toBe("36");
    expect(mark?.querySelector("img")?.getAttribute("alt")).toBe("");
  } finally {
    client.clear();
  }
});

it.each([
  ["/platform/reviews", "workspace", "Revisões"],
  ["/platform/indexing", "workspace", "Indexação"],
  ["/platform/usage", "page", "Uso"],
])("uses the %s route's shell (%s) and names the page in the top bar", (pathname, shell, title) => {
  location.pathname = pathname;
  const client = new QueryClient();
  try {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <PlatformLayout />
      </QueryClientProvider>,
    );
    expect(html).toContain(`data-shell="${shell}"`);
    expect(html).toContain('href="/platform/usage"');
    // The rail's tooltips carry every page name, so look only in the top bar.
    const topbar = new DOMParser().parseFromString(html, "text/html").querySelector("[data-topbar]");
    expect(topbar?.textContent).toContain(title);
  } finally {
    client.clear();
  }
});
