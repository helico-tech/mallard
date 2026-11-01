import * as Sentry from "@sentry/browser";
import { submitOrder } from "./checkout";

// Runtime config from the query string, so one build can talk to any
// server: ?dsn=...&release=...
const params = new URLSearchParams(window.location.search);

const release = params.get("release");
Sentry.init({
  dsn: params.get("dsn") ?? undefined,
  // Only set when given: `release: undefined` would hide the release that
  // @sentry/vite-plugin injects.
  ...(release ? { release } : {}),
  environment: "e2e-browser",
  sendClientReports: false,
});
Sentry.setTag("build", __BUILD_ID__);

declare global {
  interface Window {
    mallardFlush: () => Promise<boolean>;
  }
}
window.mallardFlush = () => Sentry.flush(10_000);

function handleCheckoutClick(): void {
  const status = document.getElementById("status")!;
  status.textContent = "submitting";
  const order = submitOrder({ items: [{ sku: "sku-42", price: 1999, qty: 2 }], coupon: params.get("coupon") ?? "NONE" });
  status.textContent = `ok ${order.total}`;
}

document.getElementById("checkout")!.addEventListener("click", handleCheckoutClick);
document.getElementById("status")!.textContent = "ready";
