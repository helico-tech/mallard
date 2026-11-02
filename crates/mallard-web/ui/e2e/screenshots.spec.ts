// README screenshots of the Web UI, from the real app and the demo data.
// Skipped unless MALLARD_SCREENSHOT_DIR is set: `scripts/screenshots.sh`.
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";

const dir = process.env.MALLARD_SCREENSHOT_DIR;

test.skip(!dir, "only for scripts/screenshots.sh");
test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2, colorScheme: "dark" });

/** Saves the page once toasts and first loads have settled, with no row under the mouse. */
async function save(page: Page, name: string) {
  await page.mouse.move(0, 0);
  await expect(page.getByText("Loading")).toHaveCount(0);
  await page.waitForTimeout(1500);
  await page.screenshot({ path: path.join(dir!, `${name}.png`) });
}

test("the story of one failed request", async ({ page }) => {
  await page.goto("/errors/api");
  const payment = page.locator("table tbody tr", { hasText: "PaymentDeclinedError" }).first();
  await expect(payment).toBeVisible();
  await save(page, "web-issues");

  await payment.click();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/issues\//);
  await expect(page.getByRole("button", { name: "Trace" })).toBeVisible();
  await save(page, "web-issue");

  await page.keyboard.press("x");
  await expect(page.getByRole("grid", { name: "Spans of the trace" })).toBeVisible();
  await save(page, "web-trace");

  await page.keyboard.press("l");
  await expect(page).toHaveURL(/\/telemetry\/logs/);
  await expect(page.getByText("card declined").first()).toBeVisible();
  await save(page, "web-logs");
});

test("the home page of an app", async ({ page }) => {
  await page.goto("/telemetry/overview?app=web");
  await expect(page.getByRole("heading", { name: "Worst operations" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "New issues" })).toBeVisible();
  await save(page, "web-home");

  // A whole id lands on its screen at once, so Cmd-K is shown open, with the apps.
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("combobox", { name: /App name/ })).toBeVisible();
  await save(page, "web-search");
});
