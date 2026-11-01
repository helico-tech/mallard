import { expect, test, type Page } from "@playwright/test";
import { OTLP, TOKEN } from "./setup";

/** A page that fails the test on any console error or CSP violation. */
function watch(page: Page): string[] {
  const problems: string[] = [];
  page.on("pageerror", (e) => problems.push(String(e)));
  page.on("console", (m) => {
    if (m.type() === "error") problems.push(m.text());
  });
  return problems;
}

const selectedRow = (page: Page) => page.locator('[aria-selected="true"]').first();

test.describe("errors tab", () => {
  test.use({ viewport: { width: 1600, height: 900 } });

  test("list, open with the keyboard, sections, back", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("j");
    await page.keyboard.press("k");
    await page.keyboard.press("Enter");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("2");
    await expect(page).toHaveURL(/section=stack/);
    await expect(page.getByText("Newest first")).toBeVisible();
    await page.keyboard.press("5");
    await expect(page).toHaveURL(/section=json/);
    await page.keyboard.press("Escape");
    await expect(page).not.toHaveURL(/\/issues\//);
  });

  test("R resolves and U brings it back: writes are capitals", async ({ page }) => {
    await page.goto("/errors/web?status=all");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    const row = page.locator("table tbody tr").first();
    await page.keyboard.press("r");
    await expect(row).not.toContainText("Resolved");
    await page.keyboard.press("R");
    await expect(row).toContainText("Resolved");
    await page.keyboard.press("U");
    await expect(row).toContainText("Unresolved");
  });

  test("older and newer events", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByText(/^Event 1\//)).toBeVisible();
    await page.keyboard.press("[");
    await expect(page.getByText(/^Event 2\//)).toBeVisible();
    await page.keyboard.press("]");
    await expect(page.getByText(/^Event 1\//)).toBeVisible();
  });

  test("x goes to the trace, and x on the span back to the error", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/issues\//);
    await expect(page.getByRole("button", { name: "Trace" })).toBeVisible();
    await page.keyboard.press("x");
    await expect(page).toHaveURL(/\/telemetry\/traces\/[0-9a-f]{32}/);
    await expect(page.getByRole("grid", { name: "Spans of the trace" })).toBeVisible();
    await page.keyboard.press("x");
    await expect(page).toHaveURL(/\/errors/);
  });
});

test.describe("telemetry tab", () => {
  test("overview drills into a service, then its traces", async ({ page }) => {
    await page.goto("/telemetry/overview");
    await page.getByRole("cell", { name: "checkout-api" }).click();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/service=checkout-api/);
    await expect(page.getByRole("heading", { name: /Operations of/ })).toBeVisible();
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/telemetry\/traces\?f=/);
  });

  test("traces: filter, a bad filter keeps the old one, E toggles errors", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("/");
    await page.keyboard.type("banana");
    await page.keyboard.press("Enter");
    await expect(page.getByRole("alert")).toContainText("unknown filter");
    await expect(page).not.toHaveURL(/f=banana/);
    await page.keyboard.press("Escape");
    await page.locator("body").press("E");
    await expect(page).toHaveURL(/f=errors/);
    await page.locator("body").press("c");
    await expect(page).not.toHaveURL(/f=/);
  });

  test("logs: severity key, follow toggle, record panel, open the trace", async ({ page }) => {
    await page.goto("/telemetry/logs");
    const log = page.getByRole("log");
    await expect(log.getByRole("row").first()).toBeVisible();
    await expect(page.getByText("Following")).toBeVisible();
    await page.keyboard.press(" ");
    await expect(page.getByText("Paused", { exact: true }).first()).toBeVisible();
    await page.keyboard.press("s");
    await expect(page).toHaveURL(/level%3Dinfo|level=info/);
    await page.keyboard.press("c");
    await log.getByRole("row").first().click();
    await expect(page.getByLabel("Log record", { exact: true })).toBeVisible();
    await page.keyboard.press("o");
    await expect(page).toHaveURL(/\/telemetry\/traces\//);
  });

  test("metrics: search, open, aggregation keys", async ({ page }) => {
    await page.goto("/telemetry/metrics");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/telemetry\/metrics\/.+/);
    await expect(page.getByRole("tab").first()).toBeVisible();
    await page.keyboard.press("a");
    await expect(page).toHaveURL(/agg=/);
    await page.keyboard.press("b");
    await expect(page.getByLabel(/Group by/)).toBeFocused();
  });

  test("hostile text from the server is shown as escapes", async ({ page, request }) => {
    const body = `e2e‮\u001b[2Jevil<img src=x onerror=alert(1)>`;
    const res = await request.post(`${OTLP}/v1/logs`, {
      data: {
        resourceLogs: [{
          resource: { attributes: [{ key: "service.name", value: { stringValue: "e2e-hostile" } }] },
          scopeLogs: [{ logRecords: [{ timeUnixNano: `${Date.now()}000000`, severityNumber: 9, body: { stringValue: body } }] }],
        }],
      },
    });
    expect(res.ok()).toBeTruthy();
    const problems = watch(page);
    await expect(async () => {
      await page.goto(`/telemetry/logs?f=${encodeURIComponent("service=e2e-hostile")}`);
      await expect(page.getByRole("log")).toContainText("\\u{202e}", { timeout: 2000 });
    }).toPass({ timeout: 20_000 });
    await expect(page.getByRole("log")).toContainText("<img src=x");
    await expect(page.locator("img")).toHaveCount(0);
    expect(problems).toEqual([]);
  });
});

test.describe("shared rules", () => {
  test.use({ timezoneId: "Asia/Tokyo" });

  test("times are UTC, in a browser in another zone", async ({ page, request }) => {
    const res = await request.get("http://127.0.0.1:38080/otel/api/v1/traces?since=1h&limit=1", { headers: { authorization: `Bearer ${TOKEN}` } });
    const [trace] = (await res.json()) as { start_time: string; trace_id: string }[];
    await page.goto(`/telemetry/traces/${trace!.trace_id}?hint=${encodeURIComponent(trace!.start_time)}`);
    await expect(page.getByText(trace!.start_time.slice(0, 19).replace("T", " "))).toBeVisible();
  });

  test("t and T walk the presets; each side keeps its own window", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page).toHaveURL(/w=1h/);
    await page.keyboard.press("t");
    await expect(page).toHaveURL(/w=6h/);
    await page.keyboard.press("T");
    await page.keyboard.press("T");
    await expect(page).toHaveURL(/w=15m/);
    await page.keyboard.press("F1");
    await expect(page).toHaveURL(/\/errors\/.*w=24h/);
    await page.keyboard.press("F2");
    await expect(page).toHaveURL(/w=15m/);
  });

  test("a toast does not take the keys away, an open dialog does", async ({ page }) => {
    await page.goto("/telemetry/logs");
    await expect(page.getByRole("log").getByRole("row").first()).toBeVisible();
    await page.keyboard.press(" ");
    await expect(page.getByRole("region").getByText("Paused")).toBeVisible();
    await page.keyboard.press("s");
    await expect(page).toHaveURL(/level/);
    await page.keyboard.press("?");
    await expect(page.getByRole("dialog", { name: "Keys" })).toBeVisible();
    const url = page.url();
    await page.keyboard.press("c");
    await page.waitForTimeout(200);
    expect(page.url()).toBe(url);
  });

  test("? opens the keys; single-key shortcuts can be turned off", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("?");
    const dialog = page.getByRole("dialog");
    await expect(dialog).toContainText("All times are UTC");
    await dialog.getByRole("switch").click();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await page.keyboard.press("t");
    await page.waitForTimeout(300);
    await expect(page).toHaveURL(/w=1h/);
  });

  const SCREENS = ["/errors", "/telemetry/overview", "/telemetry/traces", "/telemetry/logs", "/telemetry/metrics", "/telemetry/status"];
  const SIZES = [{ width: 1440, height: 900 }, { width: 1280, height: 720 }, { width: 1024, height: 700 }, { width: 390, height: 800 }];

  for (const size of SIZES) {
    test(`nothing is hidden at ${size.width}x${size.height}, no console errors or CSP violations`, async ({ page }) => {
      await page.setViewportSize(size);
      const problems = watch(page);
      for (const url of SCREENS) {
        await page.goto(url);
        await expect(page.locator("main")).toBeVisible();
        await page.waitForTimeout(800);
        const found = await page.evaluate(() => {
          const bad: string[] = [];
          const vis = (e: Element) => {
            const r = e.getBoundingClientRect();
            return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== "hidden";
          };
          if (document.documentElement.scrollWidth > innerWidth) bad.push("the page scrolls sideways");
          const main = document.querySelector("main")!;
          // The first visible column of every table has real room.
          for (const t of main.querySelectorAll("table")) {
            const widths = [...(t.querySelector("tbody tr")?.children ?? [])].filter(vis).map((c) => c.getBoundingClientRect().width);
            for (const w of widths) if (w < 40) bad.push(`a column is only ${Math.round(w)}px wide`);
            const heads = [...t.querySelectorAll("thead th")].filter(vis);
            const cells = [...(t.querySelector("tbody tr")?.children ?? [])].filter(vis);
            if (cells.length && heads.length !== cells.length) bad.push(`head has ${heads.length} columns, body ${cells.length}`);
          }
          // Controls are inside the window, or inside something that scrolls.
          for (const e of main.querySelectorAll("button,input:not([tabindex='-1']):not([aria-hidden]),a,[role=tab]")) {
            if (!vis(e)) continue;
            const r = e.getBoundingClientRect();
            let scroller = false;
            for (let p = e.parentElement; p; p = p.parentElement) {
              const o = getComputedStyle(p).overflowX;
              if ((o === "auto" || o === "scroll") && p.scrollWidth > p.clientWidth) scroller = true;
            }
            if (!scroller && (r.right > innerWidth + 1 || r.left < -1)) bad.push(`off screen: ${(e as HTMLElement).innerText || e.getAttribute("aria-label")}`);
          }
          return bad;
        });
        expect(found, url).toEqual([]);
      }
      expect(problems).toEqual([]);
    });
  }
});

test.describe("long values and reloads", () => {
  const long = "payment-reconciliation-settlement-orchestrator-eu-west-1-production-canary-blue-very-long-service-name";

  test("a very long service name hides nothing", async ({ page, request }) => {
    const now = `${Date.now()}000000`;
    const trace = "ab".repeat(16);
    const res = await request.post(`${OTLP}/v1/traces`, {
      data: { resourceSpans: [{ resource: { attributes: [{ key: "service.name", value: { stringValue: long } }] }, scopeSpans: [{ spans: [
        { traceId: trace, spanId: "cd".repeat(8), name: `operation-${"x".repeat(160)}`, kind: 2, startTimeUnixNano: now, endTimeUnixNano: `${Date.now() + 50}000000`, status: {} },
      ] }] }] },
    });
    expect(res.ok()).toBeTruthy();
    await expect(async () => {
      await page.goto("/telemetry/overview");
      await expect(page.getByRole("cell", { name: long })).toBeVisible({ timeout: 2000 });
    }).toPass({ timeout: 20_000 });
    // Every column of the row is still inside its card.
    const inside = await page.evaluate(() => {
      const table = document.querySelector("main table")!;
      const box = table.parentElement!.getBoundingClientRect();
      return { table: table.getBoundingClientRect().width <= box.width + 1, sideways: document.documentElement.scrollWidth <= innerWidth };
    });
    expect(inside).toEqual({ table: true, sideways: true });
    await page.goto(`/telemetry/traces/${trace}`);
    const panel = page.getByLabel("Span details");
    await expect(panel).toBeVisible();
    const clipped = await panel.evaluate((e) => [...e.querySelectorAll("h3,dd")].some((x) => x.getBoundingClientRect().right > e.getBoundingClientRect().right + 1));
    expect(clipped).toBe(false);
  });

  test("a trace deeper than the call stack still shows", async ({ page, request }) => {
    const n = 8000;
    const trace = "de".repeat(16);
    const start = Date.now();
    const id = (i: number) => (i + 1).toString(16).padStart(16, "0");
    const spans = Array.from({ length: n }, (_, i) => ({
      traceId: trace,
      spanId: id(i),
      ...(i ? { parentSpanId: id(i - 1) } : {}),
      name: `deep-${i}`,
      kind: 1,
      startTimeUnixNano: `${start}000000`,
      endTimeUnixNano: `${start + 5}000000`,
      status: {},
    }));
    const res = await request.post(`${OTLP}/v1/traces`, {
      data: { resourceSpans: [{ resource: { attributes: [{ key: "service.name", value: { stringValue: "deep" } }] }, scopeSpans: [{ spans }] }] },
    });
    expect(res.ok()).toBeTruthy();
    await expect(async () => {
      await page.goto(`/telemetry/traces/${trace}`);
      await expect(page.getByText(`${(n / 1000).toFixed(0)}k spans`)).toBeVisible({ timeout: 3000 });
    }).toPass({ timeout: 30_000 });
    await expect(page.getByRole("grid", { name: "Spans of the trace" })).toContainText("deep-0");
  });

  test("reloading a page with dots in its address gives the app, not a 404", async ({ page }) => {
    await page.goto("/telemetry/metrics/http.server.requests?w=1h");
    await expect(page.getByRole("heading", { name: "http.server.requests" })).toBeVisible();
    await page.reload();
    await expect(page.getByRole("heading", { name: "http.server.requests" })).toBeVisible();
  });

  test("Esc goes back to where you came from, past the tabs", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("2");
    await page.keyboard.press("3");
    await page.keyboard.press("Escape");
    await expect(page).not.toHaveURL(/\/issues\//);
  });

  test("a focused menu does not swallow the keys", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.getByRole("combobox").first().focus();
    await page.keyboard.press("t");
    await expect(page).toHaveURL(/w=6h/);
  });

  test("the overview does not jump on load", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 500 });
    await page.goto("/telemetry/overview");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.waitForTimeout(500);
    expect(await page.evaluate(() => document.querySelector("main > div")!.scrollTop)).toBe(0);
  });
});

test.describe("the TUI keeps its state", () => {
  test("Enter on the Events tab shows that event", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByText(/^Event 1\//)).toBeVisible();
    await page.keyboard.press("4");
    await expect(page.locator("table tbody tr").nth(3)).toBeVisible();
    await page.keyboard.press("j");
    await page.keyboard.press("j");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/section=stack/);
    await expect(page.getByText(/^Event 3\//)).toBeVisible();
  });

  test("a and w stay on across sections, and work from any section", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("a");
    await page.keyboard.press("2");
    await expect(page.getByRole("switch", { name: /In-app only/ })).toBeChecked();
    await page.keyboard.press("1");
    await page.keyboard.press("2");
    await expect(page.getByRole("switch", { name: /In-app only/ })).toBeChecked();
  });

  test("F1 and F2 come back to the same view with its filter", async ({ page }) => {
    await page.goto("/telemetry/traces?f=errors&w=6h");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("F1");
    await expect(page.getByLabel("Search issues")).toBeVisible();
    await page.keyboard.press("F2");
    await expect(page).toHaveURL(/\/telemetry\/traces.*f=errors/);
    await expect(page.getByRole("searchbox").or(page.getByLabel("Filter traces"))).toBeVisible();
    await page.keyboard.press("3");
    await expect(page).toHaveURL(/\/telemetry\/logs/);
    await expect(page.getByLabel("Filter logs")).toBeVisible();
    await page.keyboard.press("2");
    await expect(page).toHaveURL(/\/telemetry\/traces.*f=errors/);
  });

  test("one window across the views of a side", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("3");
    await expect(page.getByLabel("Filter logs")).toBeVisible();
    await page.keyboard.press("t");
    await expect(page).toHaveURL(/w=6h/);
    await page.keyboard.press("2");
    await expect(page.getByLabel("Filter traces")).toBeVisible();
    await expect(page).toHaveURL(/w=6h/);
  });

  test("Esc from an issue returns to the list with its filters", async ({ page }) => {
    await page.goto("/errors/web?status=all&sort=count");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/status=all/);
    await expect(page).toHaveURL(/sort=count/);
  });

  test("Esc on a trace opened directly stays in the app", async ({ page, request }) => {
    const res = await request.get("http://127.0.0.1:38080/otel/api/v1/traces?since=1h&limit=1", { headers: { authorization: `Bearer ${TOKEN}` } });
    const [trace] = (await res.json()) as { trace_id: string }[];
    await page.goto(`/telemetry/traces/${trace!.trace_id}`);
    await expect(page.getByRole("grid", { name: "Spans of the trace" })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/\/telemetry\/traces(\?|$)/);
  });

  test("back follows the side's own stack, not the browser's", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("F2");
    await expect(page.getByRole("heading", { name: "Services" })).toBeVisible();
    await page.keyboard.press("F1");
    await expect(page.getByRole("tab", { name: "Stack trace" })).toBeVisible();
    await page.keyboard.press("t");
    await expect(page).toHaveURL(/w=7d/);
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/\/errors\/web\?.*w=7d/);
  });

  test("Enter in a search box applies and hands the keys back", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("/");
    await page.keyboard.type("min=1ms");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/f=min/);
    await page.keyboard.press("c");
    await expect(page).not.toHaveURL(/f=/);
  });

  test("a closed menu gives Enter back to the list", async ({ page }) => {
    await page.goto("/errors");
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    await page.keyboard.press("p");
    await expect(page.getByRole("listbox")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("listbox")).toBeHidden();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/issues\//);
  });

  test("logs have a cursor from the start: o, Enter, q", async ({ page }) => {
    await page.goto(`/telemetry/logs?f=${encodeURIComponent("service=checkout-api")}`);
    await expect(page.getByRole("log").getByRole("row").first()).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByLabel("Log record", { exact: true })).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByLabel("Log record", { exact: true })).toBeHidden();
    await page.keyboard.press("Enter");
    await expect(page.getByLabel("Log record", { exact: true })).toBeVisible();
    await page.keyboard.press("q");
    await expect(page.getByLabel("Log record", { exact: true })).toBeHidden();
    await page.keyboard.press("o");
    await expect(page).toHaveURL(/\/telemetry\/traces\//);
  });

  test("r reloads the log records", async ({ page }) => {
    await page.goto("/telemetry/logs");
    await expect(page.getByRole("log").getByRole("row").first()).toBeVisible();
    const reload = page.waitForRequest((r) => r.url().includes("/otel/api/v1/logs?") && r.url().includes("limit=1000"));
    await page.keyboard.press("r");
    await reload;
  });
});

test.describe("sign-in mode", () => {
  test.use({ baseURL: "http://127.0.0.1:38089" });

  test("a wrong token is refused; a good one works and page code cannot read it", async ({ page, context }) => {
    await page.goto("/errors");
    const input = page.getByLabel("API token");
    await input.fill("nope");
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.getByText("does not accept")).toBeVisible();
    await input.fill(TOKEN);
    await page.getByRole("button", { name: "Sign in" }).click();
    await expect(page.locator("table tbody tr").first()).toBeVisible();
    const cookie = (await context.cookies()).find((c) => c.name === "mallard_session")!;
    expect(cookie.httpOnly).toBe(true);
    expect(cookie.sameSite).toBe("Strict");
    expect(cookie.value).not.toContain(TOKEN);
    expect(atob(cookie.value.replaceAll("-", "+").replaceAll("_", "/"))).not.toContain(TOKEN);
    expect(await page.evaluate(() => document.cookie)).not.toContain("mallard_session");
    await page.getByRole("button", { name: "Sign out" }).click();
    await expect(page.getByLabel("API token")).toBeVisible();
  });
});

test.describe("first screen", () => {
  test("selection stays on a row by id across a refresh", async ({ page }) => {
    await page.goto("/telemetry/traces");
    await expect(page.locator("table tbody tr").nth(3)).toBeVisible();
    await page.keyboard.press("j");
    await page.keyboard.press("j");
    await expect(selectedRow(page)).toBeVisible();
    const id = await selectedRow(page).getAttribute("data-row-id");
    await page.keyboard.press("r");
    await page.waitForTimeout(800);
    await expect(page.locator(`[data-row-id="${id}"]`)).toHaveAttribute("aria-selected", "true");
  });
});
