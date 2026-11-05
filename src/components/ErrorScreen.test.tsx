import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { ErrorScreen } from "./ErrorScreen";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
window.scrollTo = () => {};
let container: HTMLDivElement;
afterEach(() => container?.remove());

let broken = true;
function Page() {
  if (broken) throw new Error("Cannot read properties of undefined (reading 'analyzing')");
  return <p>Recovered</p>;
}

it("replaces a crashed page with a recoverable error screen", async () => {
  broken = true;
  const root = createRootRoute();
  const route = createRoute({ getParentRoute: () => root, path: "/platform/skills", component: Page });
  const reviews = createRoute({
    getParentRoute: () => root,
    path: "/platform/reviews",
    component: () => <p>Reviews</p>,
  });
  const router = createRouter({
    routeTree: root.addChildren([route, reviews]),
    history: createMemoryHistory({ initialEntries: ["/platform/skills"] }),
    defaultErrorComponent: ErrorScreen,
  });
  vi.spyOn(console, "error").mockImplementation(() => {});
  container = document.createElement("div");
  document.body.append(container);
  const view = createRoot(container);
  await act(async () => view.render(<RouterProvider router={router} />));
  await vi.waitFor(() =>
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("Esta página não pôde ser mostrada"),
  );
  expect(container.querySelector("pre")?.textContent).toContain("reading 'analyzing'");
  expect(container.querySelector('a[href="/platform/reviews"]')?.textContent).toBe("Ir para Revisões");
  broken = false;
  await act(async () =>
    [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("Tentar de novo"))!.click(),
  );
  await vi.waitFor(() => expect(container.textContent).toContain("Recovered"));
  act(() => view.unmount());
});
