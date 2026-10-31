import * as Sentry from "@sentry/node";
Sentry.init({ dsn: process.env.DSN, release: "web@1.2.3", environment: "e2e", tracesSampleRate: 1.0 });
Sentry.setUser({ id: "user-42", email: "u@example.com" });
Sentry.setTag("tenant", "acme");
function parseConfig(input) { return JSON.parse(input).settings.theme.toUpperCase(); }
try { parseConfig('{"settings":{}}'); } catch (e) { Sentry.captureException(e); }
Sentry.captureMessage("Payment 123 failed", "warning");
await Sentry.startSpan({ name: "checkout" }, async () => { Sentry.captureException(new Error("inside span")); });
const ok = await Sentry.flush(5000);
console.log("flushed", ok);
