import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import { defineConfig } from "vite";
import { resolve } from "path";
import { apx, OpenAPI, Orval } from "apx/vite-plugin";
import { readMetadata, type ApxMetadata } from "apx/vite-plugin";

const { appName: APP_NAME, appModule: APP_MODULE } =
  readMetadata() as ApxMetadata;

const APP_UI_PATH = `./src/${APP_NAME}/ui`;
const OUT_DIR = `../__dist__`; // relative to APP_UI_PATH!
const OPENAPI_JSON_PATH = "node_modules/.tmp/openapi.json";

export default defineConfig({
  root: APP_UI_PATH,
  publicDir: "./public", // relative to APP_UI_PATH!
  plugins: [
    tanstackRouter({
      target: "react",
      autoCodeSplitting: true,
      routesDirectory: `${APP_UI_PATH}/routes`,
      generatedRouteTree: "./types/routeTree.gen.ts",
    }),
    apx({
      steps: [
        OpenAPI(APP_MODULE, OPENAPI_JSON_PATH),
        Orval({
          input: OPENAPI_JSON_PATH,
          output: {
            target: `${APP_UI_PATH}/lib/api.ts`,
            client: "react-query",
            httpClient: "axios",
            prettier: true,
            override: {
              query: {
                useQuery: true,
                useSuspenseQuery: true,
              },
            },
          },
        }),
      ],
      ignore: ["node_modules"],
    }),
    react(),
    tailwindcss(),
  ],
  // setup proxy for the api, only used in development
  server: {
    proxy: {
      "/api": {
        target: "http://localhost:8000",
        changeOrigin: true,
        secure: false,
      },
    },
  },
  resolve: {
    alias: {
      "@": resolve(__dirname, APP_UI_PATH),
    },
  },
  build: {
    outDir: OUT_DIR,
    emptyOutDir: true,
  },
});
