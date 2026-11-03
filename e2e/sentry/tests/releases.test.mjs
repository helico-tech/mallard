// `sentry-cli releases` and `deploys` store what they say they store (ADR 0035),
// with both major versions of the real CLI.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { TestServer } from "../lib/server.mjs";
import { sentryCliPath } from "../lib/browser.mjs";

let server;
before(async () => {
  server = await TestServer.start();
});
after(async () => {
  await server?.stop();
});

function cli(major, args, { ok = true } = {}) {
  const env = { ...process.env, SENTRY_DISABLE_UPDATE_CHECK: "true" };
  const all = ["--url", server.base, "--auth-token", server.token, ...args];
  return new Promise((resolve, reject) => {
    execFile(sentryCliPath(major), all, { env }, (err, stdout, stderr) => {
      const out = `${stdout}${stderr}`;
      if (ok && err) reject(new Error(`sentry-cli ${args.join(" ")}: ${out}`));
      else resolve({ code: err ? (err.code ?? 1) : 0, out });
    });
  });
}

async function releases(project) {
  const r = await fetch(`${server.url}/v1/projects/${project}/releases?since=1h`, {
    headers: { authorization: `Bearer ${server.token}` },
  });
  assert.equal(r.status, 200);
  return (await r.json()).releases;
}

for (const major of [3, 2]) {
  test(`sentry-cli ${major}.x: a release with no events, finalized and deployed`, async () => {
    const project = (await server.createProject(`Releases ${major}`)).slug;
    const version = `quiet@${major}`;
    const o = ["-o", "org"];

    await cli(major, ["releases", ...o, "-p", project, "new", version]);
    let [r] = await releases(project);
    assert.equal(r.version, version);
    assert.equal(r.first_seen, null);
    assert.equal(r.date_released, null);

    await cli(major, ["releases", ...o, "finalize", version]);
    await cli(major, ["deploys", ...o, "-r", version, "new", "-e", "production", "-n", "first"]);
    [r] = await releases(project);
    assert.ok(r.date_released, "finalized");
    assert.equal(r.deploys[0].environment, "production");
    assert.equal(r.deploys[0].name, "first");

    const listed = await cli(major, ["releases", ...o, "list"]);
    assert.match(listed.out, new RegExp(version.replace(".", "\\.")), "releases list shows it");
    const info = await cli(major, ["releases", ...o, "info", version]);
    assert.match(info.out, new RegExp(version), "releases info shows it");
    const deploys = await cli(major, ["deploys", ...o, "-r", version, "list"]);
    assert.match(deploys.out, /production/, "deploys list shows it");

    const unknown = await cli(major, ["releases", ...o, "-p", "no-such-project", "new", `x@${major}`], { ok: false });
    assert.notEqual(unknown.code, 0, "an unknown project fails");
    assert.match(unknown.out, /no-such-project/, unknown.out);
    const missing = await cli(major, ["releases", ...o, "info", "never-made"], { ok: false });
    assert.notEqual(missing.code, 0, `an unknown release fails: ${missing.out}`);
  });
}
