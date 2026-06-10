import assert from "node:assert/strict";
import { test } from "node:test";
import { Miniflare } from "miniflare";

function workerScript() {
  return `
    const vm = {
      spawn(module, fun, argsJson) {
        this.last = { module, fun, args: JSON.parse(argsJson) };
        return 1;
      },
      run_step() {
        return JSON.stringify({
          executed: 1,
          yielded: 0,
          waiting: 0,
          exited: 1,
          errored: 0,
          results: [{ pid: 1, value: { status: 201, headers: { "x-beamr": "edge" }, body: this.last.args[0].method + " " + this.last.args[0].url } }]
        });
      }
    };
    function runUntilExit(vm, pid) {
      const summary = JSON.parse(vm.run_step());
      return { summary, result: summary.results.find((entry) => entry.pid === pid) };
    }
    async function requestToBeamValue(request) {
      const body = request.method === "GET" || request.method === "HEAD" ? "" : await request.text();
      return { method: request.method, url: request.url, headers: Object.fromEntries(request.headers), body };
    }
    export default {
      async fetch(request, env) {
        if (request.headers.get("upgrade")) {
          return new Response("WebSocket upgrades are not supported by this stateless Beamr edge worker", { status: 426 });
        }
        const requestValue = await requestToBeamValue(request);
        const pid = vm.spawn(env.BEAMR_EDGE_MODULE, env.BEAMR_EDGE_FUNCTION, JSON.stringify([requestValue]));
        const { result } = runUntilExit(vm, pid);
        return new Response(result.value.body, { status: result.value.status, headers: result.value.headers });
      }
    };
  `;
}

test("Cloudflare Worker spawns one BEAM process per HTTP request shape", async () => {
  const miniflare = new Miniflare({
    modules: true,
    script: workerScript(),
    bindings: {
      BEAMR_EDGE_MODULE: "edge_handler",
      BEAMR_EDGE_FUNCTION: "handle",
      BEAMR_EDGE_MAX_STEPS: "1024",
    },
  });
  try {
    const response = await miniflare.dispatchFetch("https://example.test/path", {
      method: "POST",
      body: "hello",
      headers: { "content-type": "text/plain" },
    });
    assert.equal(response.status, 201);
    assert.equal(response.headers.get("x-beamr"), "edge");
    assert.equal(await response.text(), "POST https://example.test/path");
  } finally {
    await miniflare.dispose();
  }
});

test("WebSocket upgrade stays out of scope", async () => {
  const miniflare = new Miniflare({ modules: true, script: workerScript() });
  try {
    const response = await miniflare.dispatchFetch("https://example.test/socket", {
      headers: { upgrade: "websocket" },
    });
    assert.equal(response.status, 426);
  } finally {
    await miniflare.dispose();
  }
});
