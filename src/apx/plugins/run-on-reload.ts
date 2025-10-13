import { type Plugin } from "vite";
import { resolve } from "path";

export type StepAction = () => void | Promise<void>;
export type StepSpec = { name: string; action: StepAction };
export const Step = (s: StepSpec) => s;

export function runOnReload({
  steps,
  ignore = [],
}: {
  steps: StepSpec[];
  ignore?: string[];
}): Plugin {
  let timer: NodeJS.Timeout | null = null;
  let stopping = false;
  let resolvedIgnores = ignore.map((i) => resolve(__dirname, i));

  async function runAll() {
    for (const s of steps) {
      if (stopping) break;
      const start = Date.now();
      try {
        console.log(`[vite-plugin-run] ${s.name} ⏳`);
        await s.action();
        console.log(`[vite-plugin-run] ${s.name} ✓ (${Date.now() - start} ms)`);
      } catch (err) {
        console.error(`[vite-plugin-run] ${s.name} ✗`, err);
        throw err;
      }
    }
  }

  function stop() {
    if (stopping) return;
    stopping = true;
    if (timer) clearTimeout(timer);
    console.log("[vite-plugin-run] stopping...");
  }

  return {
    name: "vite-plugin-run",
    apply: () => true,
    configResolved() {
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
    },
    async buildStart() {
      await runAll();
    },
    handleHotUpdate(ctx) {
      if (resolvedIgnores.some((i) => ctx.file.includes(i))) {
        return;
      }
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => ((timer = null), void runAll()), 100);
    },
    closeBundle() {
      stop();
    },
  };
}
