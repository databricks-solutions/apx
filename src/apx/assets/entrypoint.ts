import { build, createServer, type InlineConfig, type Plugin } from "vite";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import type { IncomingMessage, ServerResponse } from "http";
import {
  LoggerProvider,
  BatchLogRecordProcessor,
} from "@opentelemetry/sdk-logs";
import { OTLPLogExporter } from "@opentelemetry/exporter-logs-otlp-http";
import { SeverityNumber } from "@opentelemetry/api-logs";
import { resourceFromAttributes } from "@opentelemetry/resources";

const APX_DEV_TOKEN_HEADER = "x-apx-dev-token";

// Parse CLI arguments (all paths are absolute, resolved by Rust)
const mode = process.argv[2]; // "dev" | "build"
const uiRoot = process.argv[3]; // Absolute path to UI source directory
const outDir = process.argv[4]; // Absolute path to build output directory
const publicDir = process.argv[5]; // Absolute path to public assets directory

// Shared config from environment
const appName = process.env.APX_APP_NAME!;

// ============================================================================
// OpenTelemetry Logging Setup
// ============================================================================
// Logs are sent directly to flux via OTLP HTTP, NOT piped through apx stdout.
// This ensures proper service attribution and avoids log interleaving issues.
// ============================================================================

let otelLogger: ReturnType<LoggerProvider["getLogger"]> | null = null;

function initOtelLogging() {
  const otelEndpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
  const serviceName = process.env.OTEL_SERVICE_NAME;
  const appPath = process.env.APX_APP_PATH;

  if (!otelEndpoint || !serviceName) {
    // OTEL not configured (e.g., during build), skip initialization
    return;
  }

  const resource = resourceFromAttributes({
    "service.name": serviceName,
    "apx.app_path": appPath || "",
  });

  const logExporter = new OTLPLogExporter({
    url: `${otelEndpoint}/v1/logs`,
  });

  const loggerProvider = new LoggerProvider({
    resource,
    processors: [
      new BatchLogRecordProcessor(logExporter, {
        maxExportBatchSize: 50,
        scheduledDelayMillis: 500,
      }),
    ],
  });

  otelLogger = loggerProvider.getLogger("apx-frontend");

  // Flush logs on exit
  const flushAndExit = async (code: number) => {
    await loggerProvider.forceFlush();
    await loggerProvider.shutdown();
    process.exit(code);
  };

  process.on("beforeExit", () => flushAndExit(0));
}

function emitLog(
  severity: "INFO" | "WARN" | "ERROR",
  message: string,
  attributes?: Record<string, string>,
) {
  const severityNumber =
    severity === "ERROR"
      ? SeverityNumber.ERROR
      : severity === "WARN"
        ? SeverityNumber.WARN
        : SeverityNumber.INFO;

  if (otelLogger) {
    otelLogger.emit({
      severityNumber,
      severityText: severity,
      body: message,
      attributes,
    });
  }
}

function log(message: string) {
  // Write to stdout for local visibility
  process.stdout.write(message + "\n");
  // Send to flux
  emitLog("INFO", message);
}

function logError(message: string) {
  // Write to stderr for local visibility
  process.stderr.write(message + "\n");
  // Send to flux
  emitLog("ERROR", message);
}

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
            log(`Redirecting to: ${redirectUrl}`);
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
  // Initialize OTEL logging before anything else
  initOtelLogging();

  const frontendPort = parseInt(process.env.APX_FRONTEND_PORT!);
  const devServerPort = parseInt(process.env.APX_DEV_SERVER_PORT!);
  const devServerHost = process.env.APX_DEV_SERVER_HOST!;

  log("Starting frontend dev server...");
  log(
    `Config: port=${frontendPort}, devServerPort=${devServerPort}, devServerHost=${devServerHost}`,
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

  log("Creating vite server...");
  const server = await createServer(config);

  log("Starting to listen...");
  await server.listen();

  server.printUrls();
  log(`[READY] Frontend server listening on port ${frontendPort}`);

  // Handle graceful shutdown
  const shutdown = async (signal: string) => {
    log(`Received ${signal}, shutting down frontend server...`);
    try {
      await server.close();
      log("Frontend server closed gracefully");
      process.exit(0);
    } catch (err) {
      logError(`Error during shutdown: ${err}`);
      process.exit(1);
    }
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  // Log if the server has any errors after startup
  server.httpServer?.on("error", (err) => {
    logError(`HTTP server error: ${err}`);
  });

  server.httpServer?.on("close", () => {
    log("HTTP server closed");
  });
}

async function runBuild() {
  const config = createBaseConfig();
  await build(config);
}

// Main entry point
if (mode === "dev") {
  runDev().catch((err) => {
    logError(`Failed to start dev server: ${err}`);
    process.exit(1);
  });
} else if (mode === "build") {
  runBuild().catch((err) => {
    logError(`Failed to build: ${err}`);
    process.exit(1);
  });
} else {
  logError(`Invalid mode: ${mode}. Expected "dev" or "build".`);
  process.exit(1);
}
