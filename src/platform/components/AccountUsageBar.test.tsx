import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import type { AiAccount } from "../../types";
import { AccountUsageBar } from "./AccountUsageBar";

vi.mock("../../services/setupApi", () => ({ setupApi: {} }));

const account: AiAccount = {
  id: 1,
  provider: "claude",
  label: "Work",
  priority: 1,
  enabled: true,
  status: "ok",
  limitedUntil: null,
  lastError: null,
  lastUsedAt: null,
  createdAt: 0,
  signedIn: true,
};

const render = (selected: AiAccount) => {
  const client = new QueryClient();
  client.setQueryData(
    ["accounts", "usage"],
    [
      {
        accountId: 1,
        windows: [
          { windowMins: 300, usedPercent: 92, resetsAt: null },
          { windowMins: 10_080, usedPercent: 40, resetsAt: null },
        ],
      },
      { accountId: 2, windows: [{ windowMins: 300, usedPercent: 10, resetsAt: null }] },
    ],
  );
  try {
    return renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <AccountUsageBar account={selected} />
      </QueryClientProvider>,
    );
  } finally {
    client.clear();
  }
};

it("shows each window of the selected account only", () => {
  const html = render(account);
  expect(html).toContain(
    'aria-label="5 horas de uso restante" aria-valuemin="0" aria-valuemax="100" aria-valuenow="8"',
  );
  expect(html).toContain('aria-valuenow="60"');
  expect(html).toContain("bg-danger");
  expect(html).not.toContain('aria-valuenow="90"');
});

it("empties the bars while the account rests after a limit", () => {
  const html = render({ ...account, status: "limited", limitedUntil: Date.now() + 60_000 });
  expect(html.match(/aria-valuenow="0"/g)).toHaveLength(2);
});

it("stays hidden until the provider shares usage", () => {
  expect(render({ ...account, id: 3 })).toBe("");
});
