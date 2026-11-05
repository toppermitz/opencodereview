import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { Toaster, toast } from "./Toast";

afterEach(() => {
  vi.useRealTimers();
});

it("shows a toast and hides it after a few seconds", async () => {
  vi.useFakeTimers();
  const container = document.createElement("div");
  const root = createRoot(container);
  await act(async () => root.render(<Toaster />));
  await act(async () => toast("Alterações salvas"));
  expect(container.querySelector('[role="status"]')?.textContent).toBe("Alterações salvas");
  await act(async () => vi.advanceTimersByTime(3500));
  expect(container.querySelector('[role="status"]')).toBeNull();
  await act(async () => root.unmount());
});
