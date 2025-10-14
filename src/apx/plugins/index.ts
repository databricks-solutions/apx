import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { type Plugin } from "vite";
import { exec } from "child_process";
import { promisify } from "util";

const execAsync = promisify(exec);

export type StepAction = string | (() => void | Promise<void>);

export type StepSpec = {
  name: string;
  action: StepAction;
};

export const Step = (spec: StepSpec): StepSpec => spec;

export interface ApxPluginOptions {
  steps?: StepSpec[];
  ignore?: string[];
}

export function apx(options: ApxPluginOptions = {}): Plugin {
  const { steps = [], ignore = [] } = options;

  let outDir: string;
  let timer: NodeJS.Timeout | null = null;
  let stopping = false;
  let resolvedIgnores: string[] = [];
  let isServeMode = false;

  async function executeAction(action: StepAction): Promise<void> {
    if (typeof action === "string") {
      // Execute as shell command
      const { stdout, stderr } = await execAsync(action);
      if (stdout) console.log(stdout.trim());
      if (stderr) console.error(stderr.trim());
    } else {
      // Execute as function
      await action();
    }
  }

  async function runAllSteps(): Promise<void> {
    for (const step of steps) {
      if (stopping) break;
      const start = Date.now();
      try {
        console.log(`[apx] ${step.name} ⏳`);
        await executeAction(step.action);
        console.log(`[apx] ${step.name} ✓ (${Date.now() - start} ms)`);
      } catch (err) {
        console.error(`[apx] ${step.name} ✗`, err);
        throw err;
      }
    }
  }

  function ensureGitignoreInOutDir(): void {
    if (!outDir) return;

    // Create the output directory if it doesn't exist
    if (!existsSync(outDir)) {
      mkdirSync(outDir, { recursive: true });
    }

    // Ensure .gitignore exists in output directory
    const gitignorePath = join(outDir, ".gitignore");
    if (!existsSync(gitignorePath)) {
      writeFileSync(gitignorePath, "*\n");
      console.log(`[apx] ensured ${gitignorePath}`);
    }
  }

  function stop(): void {
    if (stopping) return;
    stopping = true;
    if (timer) clearTimeout(timer);
    console.log("[apx] stopping...");
  }

  function reset(): void {
    stopping = false;
    timer = null;
  }

  return {
    name: "apx",
    apply: () => true,

    configResolved(config) {
      outDir = config.build.outDir;
      isServeMode = config.command === "serve";
      resolvedIgnores = ignore.map((pattern) =>
        resolve(process.cwd(), pattern),
      );

      // Reset state for new build
      reset();

      // Setup signal handlers for graceful shutdown
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },

    configureServer(server) {
      server.httpServer?.once("close", stop);
    },

    async buildStart() {
      // Only ensure gitignore in serve mode at start
      // In build mode, we'll do it after files are written
      if (isServeMode) {
        ensureGitignoreInOutDir();
      }

      if (steps.length > 0) {
        await runAllSteps();
      }
    },

    handleHotUpdate(ctx) {
      // Check if file should be ignored
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }

      // Debounce step execution on HMR updates
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void runAllSteps();
      }, 100);
    },

    writeBundle() {
      // In build mode, ensure gitignore after all files are written
      if (!isServeMode) {
        ensureGitignoreInOutDir();
      }
    },

    closeBundle() {
      stop();
    },
  };
}

// Default export for convenience: import apx from "apx"
export default apx;
