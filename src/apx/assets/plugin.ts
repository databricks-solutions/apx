import { readFileSync } from "fs";
import { join, resolve } from "path";
import { parse } from "smol-toml";
import type { IncomingMessage, ServerResponse } from "http";
import type { Plugin } from "vite";

const APX_DEV_TOKEN_HEADER = "x-apx-dev-token";
const APX_SIGNAL_PATH = "/_apx/signal";

type ApxMetadata = {
  appName: string;
  appSlug: string;
  appModule: string;
};

// Read metadata from pyproject.toml using toml npm package
function readMetadata(): ApxMetadata {
  const pyprojectPath = join(process.cwd(), "pyproject.toml");
  const pyproject = parse(readFileSync(pyprojectPath, "utf-8")) as any;

  const metadata = pyproject?.tool?.apx?.metadata;

  if (!metadata || typeof metadata !== "object") {
    throw new Error("Could not find [tool.apx.metadata] in pyproject.toml");
  }

  return {
    appName: metadata["app-name"],
    appSlug: metadata["app-slug"],
    appModule: metadata["app-module"],
  };
}

// Get port configuration from environment variables (set by APX dev server)
function getPortConfig(): {
  frontendPort: number;
  devServerPort: number;
  devServerHost: string;
  host: string;
} {
  const frontendPortEnv = process.env.APX_FRONTEND_PORT;
  const devServerPortEnv = process.env.APX_DEV_SERVER_PORT;
  const devServerHostEnv = process.env.APX_DEV_SERVER_HOST;

  if (!frontendPortEnv) {
    throw new Error(
      "APX_FRONTEND_PORT environment variable is not set. " +
        "Please start the development server using 'apx dev' command.",
    );
  }
  if (!devServerPortEnv) {
    throw new Error(
      "APX_DEV_SERVER_PORT environment variable is not set. " +
        "Please start the development server using 'apx dev' command.",
    );
  }

  const frontendPort = parseInt(frontendPortEnv, 10);
  if (isNaN(frontendPort)) {
    throw new Error(
      `Invalid APX_FRONTEND_PORT value: ${frontendPortEnv}. Expected a number.`,
    );
  }
  const devServerPort = parseInt(devServerPortEnv, 10);
  if (isNaN(devServerPort)) {
    throw new Error(
      `Invalid APX_DEV_SERVER_PORT value: ${devServerPortEnv}. Expected a number.`,
    );
  }

  return {
    frontendPort,
    devServerPort,
    devServerHost: devServerHostEnv || "localhost",
    host: "localhost",
  };
}

// Main APX plugin that configures Vite for APX apps
export function apxPlugin(): Plugin {
  let isDevServer = false;

  return {
    name: "apx-plugin",
    config(_, { command }) {
      const { appName: APP_NAME, appSlug: APP_SLUG } = readMetadata();

      const APP_UI_PATH = `./src/${APP_SLUG}/ui`;
      const OUT_DIR = `../__dist__`; // relative to APP_UI_PATH!

      // Port config is only needed for dev server, not for production build
      isDevServer = command === "serve";
      const serverConfig = isDevServer
        ? (() => {
            const { frontendPort } = getPortConfig();
            return {
              host: "localhost",
              port: frontendPort,
              strictPort: true,
              // Configure HMR to connect directly to Vite instead of through the APX proxy.
              // This avoids WebSocket proxy issues and makes HMR more reliable.
              hmr: {
                host: "localhost",
                port: frontendPort,
                // clientPort tells the browser to connect directly to Vite's port
                clientPort: frontendPort,
              },
            };
          })()
        : undefined;

      return {
        root: APP_UI_PATH,
        publicDir: "./public", // relative to APP_UI_PATH!
        server: serverConfig,
        resolve: {
          alias: {
            "@": resolve(process.cwd(), APP_UI_PATH),
          },
        },
        build: {
          outDir: OUT_DIR,
          emptyOutDir: true,
        },
        define: {
          __APP_NAME__: JSON.stringify(APP_NAME),
        },
      };
    },
    configureServer(server) {
      if (!isDevServer) return;

      console.log(
        "[APX] APX_DEV_SERVER_PORT:",
        process.env.APX_DEV_SERVER_PORT,
      );
      console.log(
        "[APX] APX_DEV_SERVER_HOST:",
        process.env.APX_DEV_SERVER_HOST,
      );
      console.log("[APX] APX_FRONTEND_PORT:", process.env.APX_FRONTEND_PORT);

      // Add middleware at the start to check for the dev token header
      server.middlewares.use(
        (req: IncomingMessage, res: ServerResponse, next) => {
          const url = req.url || "";

          if (url.startsWith(APX_SIGNAL_PATH)) {
            const devToken = process.env.APX_DEV_TOKEN;
            const requestToken = req.headers[APX_DEV_TOKEN_HEADER] as
              | string
              | undefined;
            if (!devToken || requestToken !== devToken) {
              console.log("[APX] received invalid token, returning 401");
              res.statusCode = 401;
              res.setHeader("Content-Type", "text/plain");
              res.end("Invalid APX dev token.");
              return;
            }

            console.log("[APX] received shutdown signal, shutting down");
            res.statusCode = 200;
            res.end("ok");
            setTimeout(() => {
              process.exit(0);
            }, 50);
            return;
          }

          // Allow internal Vite requests (HMR, etc.)
          if (
            url.startsWith("/@") ||
            url.startsWith("/__vite") ||
            url.startsWith("/node_modules")
          ) {
            next();
            return;
          }

          // Allow WebSocket upgrade requests (HMR connections from the proxy)
          const upgradeHeader = req.headers["upgrade"];
          if (
            upgradeHeader &&
            upgradeHeader.toLowerCase().includes("websocket")
          ) {
            next();
            return;
          }

          // Check for the APX dev token header
          const devToken = process.env.APX_DEV_TOKEN;
          const requestToken = req.headers[APX_DEV_TOKEN_HEADER] as
            | string
            | undefined;
          const hasValidToken = devToken && requestToken === devToken;

          if (!hasValidToken) {
            // Redirect to APX dev server instead of returning 403
            const devServerPort = process.env.APX_DEV_SERVER_PORT;
            const devServerHost =
              process.env.APX_DEV_SERVER_HOST || "localhost";
            const hostHeader = req.headers.host;
            const requestHost = hostHeader?.split(":")[0] || "localhost";
            const redirectHost =
              devServerHost === "0.0.0.0" ? requestHost : devServerHost;

            if (devServerPort) {
              const redirectUrl = `http://${redirectHost}:${devServerPort}${url}`;
              console.log(`[APX] Redirecting to: ${redirectUrl}`);
              res.statusCode = 302;
              res.setHeader("Location", redirectUrl);
              res.end();
            } else {
              console.log("[APX] No dev server port, returning 403");
              // Fallback to 403 if dev server port is not set
              res.statusCode = 403;
              res.setHeader("Content-Type", "text/plain");
              res.end(
                "Direct access to Vite dev server is not allowed. " +
                  "Please access through the APX dev server proxy.",
              );
            }
            return;
          }

          console.log("[APX] Proxy header present, passing through");
          next();
        },
      );
    },
  };
}
