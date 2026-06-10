import { createPreloadedVm, runUntilExit } from "../../../crates/beamr-wasm/pkg/beamr.bundle.mjs";

const DEFAULT_MODULE = "edge_handler";
const DEFAULT_FUNCTION = "handle";
const DEFAULT_MAX_STEPS = 1024;

let preloadedVmPromise;

function configuredModule(env) {
  return env.BEAMR_EDGE_MODULE || DEFAULT_MODULE;
}

function configuredFunction(env) {
  return env.BEAMR_EDGE_FUNCTION || DEFAULT_FUNCTION;
}

function configuredMaxSteps(env) {
  const value = Number(env.BEAMR_EDGE_MAX_STEPS || DEFAULT_MAX_STEPS);
  return Number.isFinite(value) && value > 0 ? value : DEFAULT_MAX_STEPS;
}

function getPreloadedVm() {
  if (!preloadedVmPromise) {
    preloadedVmPromise = createPreloadedVm();
  }
  return preloadedVmPromise;
}

function headersToObject(headers) {
  const object = {};
  for (const [name, value] of headers) {
    object[name] = value;
  }
  return object;
}

async function requestToBeamValue(request) {
  const body = request.method === "GET" || request.method === "HEAD" ? "" : await request.text();
  return {
    method: request.method,
    url: request.url,
    headers: headersToObject(request.headers),
    body,
  };
}

function jsonSummary(value) {
  return typeof value === "string" ? JSON.parse(value) : value;
}

function responseFromBeamValue(value) {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const status = Number(value.status || 200);
    const headers = value.headers && typeof value.headers === "object" ? value.headers : {};
    const body = value.body == null ? "" : value.body;
    return new Response(typeof body === "string" ? body : JSON.stringify(body), { status, headers });
  }
  if (typeof value === "string") {
    return new Response(value, { status: 200 });
  }
  return Response.json(value);
}

async function runBeamRequest(request, env) {
  const { vm } = await getPreloadedVm();
  const requestValue = await requestToBeamValue(request);
  const pid = vm.spawn(configuredModule(env), configuredFunction(env), JSON.stringify([requestValue]));
  const { summary, result } = runUntilExit(vm, pid, { maxSteps: configuredMaxSteps(env) });
  if (!result) {
    return Response.json(
      { error: "beam process did not produce a response", summary: jsonSummary(summary) },
      { status: 503 }
    );
  }
  return responseFromBeamValue(result.value);
}

export default {
  async fetch(request, env = {}) {
    if (request.headers.get("upgrade")) {
      return new Response("WebSocket upgrades are not supported by this stateless Beamr edge worker", {
        status: 426,
      });
    }
    try {
      return await runBeamRequest(request, env);
    } catch (error) {
      return Response.json(
        { error: error instanceof Error ? error.message : String(error) },
        { status: 500 }
      );
    }
  },
};
