import { loadConfig } from "./config.js";
import { createSignalingServer } from "./server.js";

const config = loadConfig();
const server = createSignalingServer(config);

const relayMode = config.turnUrls.length > 0 ? "STUN + TURN" : "STUN only (no relay fallback)";
console.log(`[rhevia-signaling] listening on ${config.host}:${config.port} — ${relayMode}`);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    console.log(`[rhevia-signaling] ${signal}, shutting down`);
    void server.close().then(() => process.exit(0));
  });
}
