import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import { defineConfig } from "vite";
import { readFileSync } from "fs";
import { join, resolve } from "path";
import { parse } from "smol-toml";

type ApxMetadata = {
  appName: string;
  appSlug: string;
  appModule: string;
};

// read metadata from pyproject.toml using toml npm package
export function readMetadata(): ApxMetadata {
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

const { appName: APP_NAME, appSlug: APP_SLUG } = readMetadata() as ApxMetadata;

const APP_UI_PATH = `./src/${APP_SLUG}/ui`;
const OUT_DIR = `../__dist__`; // relative to APP_UI_PATH!
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
  define: {
    __APP_NAME__: JSON.stringify(APP_NAME),
  },
});
