import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { type Plugin } from "vite";
import { exec } from "child_process";
import { promisify } from "util";
import { generate, type OptionsExport as OrvalConfig } from "orval";

const execAsync = promisify(exec);

// Re-export OrvalConfig for convenience
export type { OrvalConfig };

export type StepAction = string | (() => void | Promise<void>);

export type StepSpec = {
  name: string;
  action: StepAction;
};

export const Step = (spec: StepSpec): StepSpec => spec;

/**
 * Predefined step for generating OpenAPI schema
 * @param appModule - The Python module path (e.g., "sample.api.app:app")
 * @param outputPath - Where to write the OpenAPI JSON file
 */
export const OpenAPI = (appModule: string, outputPath: string): StepSpec => ({
  name: "openapi",
  action: `uv run apx openapi ${appModule} ${outputPath}`,
});

/**
 * Predefined step for generating API client with Orval
 * @param config - Orval configuration object
 */
export const Orval = (config: OrvalConfig): StepSpec => ({
  name: "orval",
  action: () => generate(config),
});
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
  let isRunningSteps = false;

  async function executeAction(action: StepAction): Promise<void> {
    if (stopping) {
      console.log(`[apx] skipping action (stopping): ${action}`);
      return;
    }
    
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
    if (stopping) {
      console.log(`[apx] skipping steps (stopping)`);
      return;
    }
    
    if (isRunningSteps) {
      console.log(`[apx] steps already running, skipping`);
      return;
    }
    
    isRunningSteps = true;
    console.log(`[apx] starting ${steps.length} step(s)`);
    
    try {
      for (const step of steps) {
        if (stopping) {
          console.log(`[apx] stopping steps early`);
          break;
        }
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
    } finally {
      isRunningSteps = false;
      console.log(`[apx] finished running steps`);
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
      }

      // Always ensure .gitignore exists in output directory
      const gitignorePath = join(outDir, ".gitignore");
      if (!existsSync(gitignorePath)) {
        writeFileSync(gitignorePath, "*\n");
      }
    } catch (err) {
      console.error(`[apx] failed to ensure output directory:`, err);
    }
  }

  function stop(): void {
    if (stopping) {
      console.log("[apx] already stopping, ignoring");
      return;
    }
    console.log("[apx] stop() called");
    stopping = true;
    if (timer) {
      console.log("[apx] clearing pending timer");
      clearTimeout(timer);
      timer = null;
    }
    console.log("[apx] stopped");
  }

  function reset(): void {
    console.log("[apx] reset() called");
    stopping = false;
    timer = null;
    isRunningSteps = false;
  }

  return {
    name: "apx",
    apply: () => true,

    configResolved(config) {
      console.log("[apx] configResolved() called");
      outDir = resolve(config.root, config.build.outDir);
      console.log(`[apx] outDir resolved to: ${outDir}`);
      resolvedIgnores = ignore.map((pattern) =>
        resolve(process.cwd(), pattern),
      );

      // Reset state for new build
      reset();

      // Ensure directory exists as soon as we know the outDir
      ensureOutDirAndGitignore();
    },

    configureServer(server) {
      console.log("[apx] configureServer() called");
      server.httpServer?.once("close", () => {
        console.log("[apx] server.httpServer 'close' event fired");
        stop();
      });

      // Ensure directory exists when server starts
      ensureOutDirAndGitignore();
    },

    async buildStart() {
      console.log("[apx] buildStart() called");
      // Ensure directory exists before build starts
      ensureOutDirAndGitignore();

      if (steps.length > 0) {
        await runAllSteps();
      }
    },

    handleHotUpdate(ctx) {
      console.log(`[apx] handleHotUpdate() called for: ${ctx.file}`);
      // Ensure directory exists on every HMR update
      ensureOutDirAndGitignore();

      // Check if file should be ignored
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        console.log(`[apx] file ignored: ${ctx.file}`);
        return;
      }

      // Debounce step execution on HMR updates
      if (timer) {
        console.log("[apx] clearing existing timer");
        clearTimeout(timer);
      }
      console.log("[apx] setting timer for step execution");
      timer = setTimeout(async () => {
        console.log("[apx] timer fired, running steps");
        timer = null;

        // Ensure directory exists before running steps
        ensureOutDirAndGitignore();
        await runAllSteps();

        // Ensure directory exists after running steps
        ensureOutDirAndGitignore();
      }, 100);
      
      // Allow the process to exit even if this timer is pending
      timer.unref();
    },

    writeBundle() {
      console.log("[apx] writeBundle() called");
      // Ensure directory exists after files are written
      ensureOutDirAndGitignore();
    },

    closeBundle() {
      console.log("[apx] closeBundle() called");
      // Ensure directory exists one final time
      ensureOutDirAndGitignore();
      stop();
    },
  };
}

// Default export for convenience: import apx from "apx"
export default apx;
