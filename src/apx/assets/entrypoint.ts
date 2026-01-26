import { build, createServer, type InlineConfig, type Plugin } from "vite";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import type { IncomingMessage, ServerResponse } from "http";

const APX_DEV_TOKEN_HEADER = "x-apx-dev-token";

// Parse CLI arguments (all paths are absolute, resolved by Rust)
const mode = process.argv[2]; // "dev" | "build"
const uiRoot = process.argv[3]; // Absolute path to UI source directory
const outDir = process.argv[4]; // Absolute path to build output directory
const publicDir = process.argv[5]; // Absolute path to public assets directory

// Shared config from environment
const appName = process.env.APX_APP_NAME!;

function getBrowserLoggingScript(): string {
  return `
(() => {
  const endpoint = "/_apx/logs";

  function sendLog(payload) {
    const body = JSON.stringify(payload);
    if (navigator.sendBeacon) {
      const blob = new Blob([body], { type: "application/json" });
      navigator.sendBeacon(endpoint, blob);
    } else {
      fetch(endpoint, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body,
        keepalive: true,
      }).catch(() => {});
    }
  }

  function formatError(error) {
    if (error instanceof Error) {
      return { message: error.message, stack: error.stack };
    }
    return { message: String(error) };
  }

  const originalError = console.error;
  console.error = (...args) => {
    originalError.apply(console, args);
    const { message, stack } = formatError(args[0]);
    sendLog({
      level: "error",
      source: "console",
      message: args.map(String).join(" ") || message,
      stack,
      timestamp: Date.now(),
    });
  };

  window.addEventListener("error", (event) => {
    sendLog({
      level: "error",
      source: "window",
      message: event.message,
      stack: event.error?.stack,
      timestamp: Date.now(),
    });
  });

  window.addEventListener("unhandledrejection", (event) => {
    const { message, stack } = formatError(event.reason);
    sendLog({
      level: "error",
      source: "promise",
      message,
      stack,
      timestamp: Date.now(),
    });
  });
})();
`;
}

// APX Plugin - handles browser logging and dev middleware
function apxPlugin(): Plugin {
  const isDevMode = mode === "dev";

  // Dev-only config (only read env vars if in dev mode)
  let frontendPort: number;
  let devServerPort: number;
  let devServerHost: string;
  let devToken: string;

  if (isDevMode) {
    frontendPort = parseInt(process.env.APX_FRONTEND_PORT!);
    devServerPort = parseInt(process.env.APX_DEV_SERVER_PORT!);
    devServerHost = process.env.APX_DEV_SERVER_HOST!;
    devToken = process.env.APX_DEV_TOKEN!;
  }

  return {
    name: "apx-plugin",

    // Inject browser logging script in dev mode
    transformIndexHtml(html) {
      if (!isDevMode) return html;

      return {
        html,
        tags: [
          {
            tag: "script",
            attrs: { type: "module" },
            children: getBrowserLoggingScript(),
            injectTo: "head-prepend",
          },
        ],
      };
    },

    // Configure dev server middleware
    configureServer(server) {
      if (!isDevMode) return;

      server.middlewares.use(
        (req: IncomingMessage, res: ServerResponse, next) => {
          const url = req.url || "";

          // Allow internal Vite requests (HMR, etc.)
          if (
            url.startsWith("/@") ||
            url.startsWith("/__vite") ||
            url.startsWith("/node_modules")
          ) {
            next();
            return;
          }

          // Allow WebSocket upgrade requests (HMR connections)
          const upgradeHeader = req.headers["upgrade"];
          if (
            upgradeHeader &&
            upgradeHeader.toLowerCase().includes("websocket")
          ) {
            next();
            return;
          }

          // Check for the APX dev token header
          const requestToken = req.headers[APX_DEV_TOKEN_HEADER] as
            | string
            | undefined;
          const hasValidToken = devToken && requestToken === devToken;

          if (!hasValidToken) {
            // Redirect to APX dev server instead of returning 403
            const hostHeader = req.headers.host;
            const requestHost = hostHeader?.split(":")[0] || "localhost";
            const redirectHost =
              devServerHost === "0.0.0.0" ? requestHost : devServerHost;

            const redirectUrl = `http://${redirectHost}:${devServerPort}${url}`;
            console.log(`[APX] Redirecting to: ${redirectUrl}`);
            res.statusCode = 302;
            res.setHeader("Location", redirectUrl);
            res.end();
            return;
          }
          next();
        },
      );
    },
  };
}

// Create base Vite config (shared between dev and build)
function createBaseConfig(): InlineConfig {
  return {
    root: uiRoot,
    publicDir: publicDir,
    resolve: {
      alias: {
        "@": uiRoot,
      },
    },
    build: {
      outDir: outDir,
      emptyOutDir: true,
    },
    define: {
      __APP_NAME__: JSON.stringify(appName),
    },
    plugins: [
      apxPlugin(),
      tanstackRouter({
        target: "react",
        autoCodeSplitting: true,
        routesDirectory: `${uiRoot}/routes`,
        generatedRouteTree: `${uiRoot}/types/routeTree.gen.ts`,
      }),
      react(),
      tailwindcss(),
    ],
  };
}

async function runDev() {
  const frontendPort = parseInt(process.env.APX_FRONTEND_PORT!);
  const devServerPort = parseInt(process.env.APX_DEV_SERVER_PORT!);
  const devServerHost = process.env.APX_DEV_SERVER_HOST!;

  console.log("[APX] Starting frontend dev server...");
  console.log(
    "[APX] Config: port=%d, devServerPort=%d, devServerHost=%s",
    frontendPort,
    devServerPort,
    devServerHost,
  );

  const config: InlineConfig = {
    ...createBaseConfig(),
    server: {
      host: "localhost",
      port: frontendPort,
      strictPort: true,
      hmr: {
        host: "localhost",
        port: frontendPort,
        clientPort: frontendPort,
      },
    },
  };

  console.log("[APX] Creating vite server...");
  const server = await createServer(config);

  console.log("[APX] Starting to listen...");
  await server.listen();

  server.printUrls();
  console.log("[APX:READY] Frontend server listening on port", frontendPort);

  // Handle graceful shutdown
  const shutdown = async (signal: string) => {
    console.log(`[APX] Received ${signal}, shutting down frontend server...`);
    try {
      await server.close();
      console.log("[APX] Frontend server closed gracefully");
      process.exit(0);
    } catch (err) {
      console.error("[APX] Error during shutdown:", err);
      process.exit(1);
    }
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  // Log if the server has any errors after startup
  server.httpServer?.on("error", (err) => {
    console.error("[APX] HTTP server error:", err);
  });

  server.httpServer?.on("close", () => {
    console.log("[APX] HTTP server closed");
  });
}

async function runBuild() {
  const config = createBaseConfig();
  await build(config);
}

// Main entry point
if (mode === "dev") {
  runDev().catch((err) => {
    console.error("[APX] Failed to start dev server:", err);
    process.exit(1);
  });
} else if (mode === "build") {
  runBuild().catch((err) => {
    console.error("[APX] Failed to build:", err);
    process.exit(1);
  });
} else {
  console.error(`[APX] Invalid mode: ${mode}. Expected "dev" or "build".`);
  process.exit(1);
}
