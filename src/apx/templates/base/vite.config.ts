import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import { defineConfig } from "vite";
import { apxPlugin } from "./.apx/plugin";

export default defineConfig({
  plugins: [
    apxPlugin(),
    tanstackRouter({
      target: "react",
      autoCodeSplitting: true,
      routesDirectory: `./routes`,
      generatedRouteTree: "./types/routeTree.gen.ts",
    }),
    react(),
    tailwindcss(),
  ],
});
