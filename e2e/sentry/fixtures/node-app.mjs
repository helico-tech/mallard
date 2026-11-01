// A small Node "app" using the real @sentry/node SDK. Only the DSN points
// at mallard. Usage: DSN=... node node-app.mjs <scenario> [tag]
// Prints one JSON line `{"ids": {...}}` with the event ids it captured.

import * as Sentry from "@sentry/node";

const [scenario, runTag = "run"] = process.argv.slice(2);
const ids = {};

Sentry.init({
  dsn: process.env.DSN,
  release: "node-app@1.2.3",
  environment: "e2e",
  tracesSampleRate: 1.0,
  // Keep the run short and deterministic.
  sendClientReports: false,
  shutdownTimeout: 10_000,
  // "strict" makes an unhandled rejection crash the process like Node's
  // default, after the SDK captured and flushed it.
  integrations: scenario === "unhandled_rejection" ? [Sentry.onUnhandledRejectionIntegration({ mode: "strict" })] : [],
});

function parseConfig(input) {
  return JSON.parse(input).settings.theme.toUpperCase();
}

function loadSettings() {
  try {
    parseConfig('{"settings":{}}');
  } catch (e) {
    throw new TypeError("could not load settings", { cause: e });
  }
}

function startApp() {
  try {
    loadSettings();
  } catch (e) {
    throw new Error("app failed to start", { cause: e });
  }
}

function failSameWay(tag) {
  throw new Error(`repeated failure ${tag}`);
}

function chargeCard() {
  throw new Error("card declined: insufficient funds");
}

function lookupUser() {
  throw new RangeError("user id out of range");
}

function tenantFailure() {
  throw new Error("tenant quota exceeded");
}

function regressionFailure(tag) {
  throw new Error(`regression failure ${tag}`);
}

const scenarios = {
  async cause_chain() {
    try {
      startApp();
    } catch (e) {
      ids.cause = Sentry.captureException(e);
    }
  },

  async message() {
    ids.message = Sentry.captureMessage(`disk almost full ${runTag}`, "warning");
  },

  async scope() {
    Sentry.addBreadcrumb({ category: "auth", message: "user logged in", level: "info", data: { method: "password" } });
    Sentry.addBreadcrumb({ category: "cart", message: "added item sku-42", level: "debug" });
    Sentry.setUser({ id: "user-42", email: "jane@example.com", username: "jane" });
    Sentry.setTag("tenant", "acme");
    Sentry.setTag("plan", "pro");
    Sentry.setContext("order", { id: "ord-1001", total: 99.95, items: 3 });
    ids.scope = Sentry.captureException(new Error(`scoped failure ${runTag}`));
  },

  async fingerprint() {
    // Two different errors forced into one issue.
    Sentry.withScope((scope) => {
      scope.setFingerprint(["payment-failure"]);
      try {
        chargeCard();
      } catch (e) {
        ids.fp_a = Sentry.captureException(e);
      }
    });
    Sentry.withScope((scope) => {
      scope.setFingerprint(["payment-failure"]);
      try {
        lookupUser();
      } catch (e) {
        ids.fp_b = Sentry.captureException(e);
      }
    });
    // One error split per tenant with {{ default }}.
    for (const tenant of ["tenant-a", "tenant-b"]) {
      Sentry.withScope((scope) => {
        scope.setFingerprint(["{{ default }}", tenant]);
        try {
          tenantFailure();
        } catch (e) {
          ids[`split_${tenant}`] = Sentry.captureException(e);
        }
      });
    }
  },

  // Run 3 times as separate processes: the SDK's dedupe integration would
  // drop identical consecutive events inside one process.
  async repeat() {
    try {
      failSameWay(runTag);
    } catch (e) {
      ids.repeat = Sentry.captureException(e);
    }
  },

  async regression() {
    try {
      regressionFailure(runTag);
    } catch (e) {
      ids.regression = Sentry.captureException(e);
    }
  },

  async span() {
    await Sentry.startSpan({ name: "checkout", op: "task" }, async (span) => {
      ids.trace_id = span.spanContext().traceId;
      ids.span = Sentry.captureException(new Error(`failure inside span ${runTag}`));
    });
  },

  async uncaught() {
    setTimeout(function crashOnTimer() {
      throw new Error(`uncaught boom ${runTag}`);
    }, 10);
    // Keep the process alive; the SDK's onUncaughtException handler flushes and exits.
    setTimeout(() => {}, 60_000);
  },

  async unhandled_rejection() {
    setTimeout(async function rejectLater() {
      throw new Error(`unhandled rejection ${runTag}`);
    }, 10);
    setTimeout(() => {}, 60_000);
  },
};

const fn = scenarios[scenario];
if (!fn) {
  console.error(`unknown scenario ${scenario}`);
  process.exit(2);
}
await fn();
if (scenario !== "uncaught" && scenario !== "unhandled_rejection") {
  const ok = await Sentry.flush(10_000);
  console.log(JSON.stringify({ ids, flushed: ok }));
}
