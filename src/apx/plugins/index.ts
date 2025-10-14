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

  async function executeAction(action: StepAction): Promise<void> {
    console.log(`[apx] executing action: ${action}`);
    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      // Execute as shell command
      const { stdout, stderr } = await execAsync(action);
      if (stdout) console.log(stdout.trim());
      if (stderr) console.error(stderr.trim());
    } else {
      // Execute as function
      await action();
    }
    ensureOutDirAndGitignore();
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

  /**
   * Ensures the output directory exists and contains a .gitignore file.
   * This is called at multiple points to guarantee the directory is always present.
   */
  function ensureOutDirAndGitignore(): void {
    if (!outDir) {
      console.error(`[apx] outDir is not set`);
      return;
    }

    try {
      // Always ensure the output directory exists
      if (!existsSync(outDir)) {
        mkdirSync(outDir, { recursive: true });
        console.log(`[apx] created output directory: ${outDir}`);
      }

      // Always ensure .gitignore exists in output directory
      const gitignorePath = join(outDir, ".gitignore");
      if (!existsSync(gitignorePath)) {
        writeFileSync(gitignorePath, "*\n");
        console.log(`[apx] created ${gitignorePath}`);
      }
    } catch (err) {
      console.error(`[apx] failed to ensure output directory:`, err);
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
      outDir = resolve(config.root, config.build.outDir);
      resolvedIgnores = ignore.map((pattern) =>
        resolve(process.cwd(), pattern),
      );

      // Reset state for new build
      reset();

      // Setup signal handlers for graceful shutdown
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);

      // Ensure directory exists as soon as we know the outDir
      ensureOutDirAndGitignore();
    },

    configureServer(server) {
      server.httpServer?.once("close", stop);

      // Ensure directory exists when server starts
      ensureOutDirAndGitignore();
    },

    async buildStart() {
      // Ensure directory exists before build starts
      ensureOutDirAndGitignore();

      if (steps.length > 0) {
        await runAllSteps();
      }
    },

    handleHotUpdate(ctx) {
      // Ensure directory exists on every HMR update
      ensureOutDirAndGitignore();

      // Check if file should be ignored
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }

      // Debounce step execution on HMR updates
      if (timer) clearTimeout(timer);
      timer = setTimeout(async () => {
        timer = null;

        // Ensure directory exists before running steps
        ensureOutDirAndGitignore();
        await runAllSteps();

        // Ensure directory exists after running steps
        ensureOutDirAndGitignore();
      }, 100);
    },

    writeBundle() {
      // Ensure directory exists after files are written
      ensureOutDirAndGitignore();
    },

    closeBundle() {
      // Ensure directory exists one final time
      ensureOutDirAndGitignore();
      stop();
    },
  };
}

// Default export for convenience: import apx from "apx"
export default apx;
