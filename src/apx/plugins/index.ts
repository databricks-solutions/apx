import { existsSync, mkdirSync, writeFileSync, readFileSync } from "fs";
import { join, resolve } from "path";
import { type Plugin } from "vite";
import { spawn, type ChildProcess } from "child_process";
import { createHash } from "crypto";
import { generate, type OutputOptions } from "orval";

// Cache for OpenAPI spec hashes to detect changes
const specHashCache = new Map<string, string>();

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
  action: `uv run --no-sync apx openapi ${appModule} ${outputPath}`,
});

/**
 * Predefined step for generating API client with Orval
 * Skips generation if the OpenAPI spec hasn't changed since last run
 * @param input - Path to the OpenAPI spec file
 * @param output - Orval output configuration
 */
export const Orval = ({
  input,
  output,
}: {
  input: string;
  output: OutputOptions;
}): StepSpec => ({
  name: "orval",
  action: async () => {
    // Check if spec file exists
    if (!existsSync(input)) {
      console.warn(
        `[apx] OpenAPI spec not found at ${input}, skipping Orval generation`,
      );
      return;
    }

    // Read and hash the spec file
    const specContent = readFileSync(input, "utf-8");
    const specHash = createHash("sha256").update(specContent).digest("hex");

    // Check if spec has changed
    const cachedHash = specHashCache.get(input);
    if (cachedHash === specHash) {
      console.log(`[apx] OpenAPI spec unchanged, skipping Orval generation`);
      return;
    }

    // Generate API client
    await generate({
      input,
      output,
    });

    // Update cache
    specHashCache.set(input, specHash);
  },
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
  let childProcesses: ChildProcess[] = [];

  /**
   * Executes a shell command using spawn, with proper signal handling
   */
  function executeShellCommand(command: string): Promise<void> {
    return new Promise((resolve, reject) => {
      if (stopping) {
        console.log(`[apx] Skipping command (stopping): ${command}`);
        resolve();
        return;
      }

      console.log(`[apx] Executing: ${command}`);

      // Parse command into command and args
      const parts = command.split(/\s+/);
      const cmd = parts[0];
      const args = parts.slice(1);

      // Spawn process with proper signal handling
      const child = spawn(cmd, args, {
        stdio: "inherit", // Forward stdout/stderr to parent
        shell: true, // Use shell for proper command parsing
        detached: false, // Keep in same process group for signal propagation
      });

      // Track child process for cleanup
      childProcesses.push(child);

      child.on("error", (err) => {
        console.error(`[apx] Process error:`, err);
        reject(err);
      });

      child.on("exit", (code, signal) => {
        // Remove from tracking
        childProcesses = childProcesses.filter((p) => p.pid !== child.pid);

        if (signal) {
          console.log(
            `[apx] Process ${child.pid} exited with signal: ${signal}`,
          );
          resolve(); // Treat signal termination as success for cleanup scenarios
        } else if (code !== 0) {
          console.error(`[apx] Process ${child.pid} exited with code: ${code}`);
          reject(new Error(`Command failed with exit code ${code}`));
        } else {
          resolve();
        }
      });

      // If we're stopping, kill the process immediately
      if (stopping && child.pid) {
        console.log(`[apx] Killing process ${child.pid} (stopping)`);
        killProcess(child);
      }
    });
  }

  /**
   * Kills a process and all its children
   */
  function killProcess(proc: ChildProcess): void {
    if (!proc.pid) return;

    try {
      // On Unix-like systems, kill the process group
      // Negative PID kills the entire process group
      if (process.platform !== "win32") {
        process.kill(-proc.pid, "SIGTERM");
        console.log(`[apx] Sent SIGTERM to process group -${proc.pid}`);
      } else {
        // On Windows, just kill the process
        proc.kill("SIGTERM");
        console.log(`[apx] Sent SIGTERM to process ${proc.pid}`);
      }
    } catch (err) {
      console.error(`[apx] Error killing process ${proc.pid}:`, err);
      // Try forceful kill as fallback
      try {
        proc.kill("SIGKILL");
      } catch (e) {
        // Ignore errors on forceful kill
      }
    }
  }

  async function executeAction(action: StepAction): Promise<void> {
    if (stopping) {
      console.log(`[apx] Skipping action (stopping)`);
      return;
    }

    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      // Execute as shell command
      await executeShellCommand(action);
    } else {
      // Execute as function
      if (stopping) return;
      await action();
    }
    ensureOutDirAndGitignore();
  }

  async function runAllSteps(): Promise<void> {
    if (stopping) {
      console.log(`[apx] Skipping steps (stopping)`);
      return;
    }

    if (isRunningSteps) {
      console.log(`[apx] Steps already running, skipping...`);
      return;
    }

    console.log(`[apx] Running ${steps.length} step(s)...`);
    isRunningSteps = true;

    try {
      for (const step of steps) {
        if (stopping) {
          console.log(`[apx] Stopping during step execution`);
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
      console.log(`[apx] All steps completed`);
    } finally {
      isRunningSteps = false;
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
    if (stopping) return;
    console.log(`[apx] Stopping... (${childProcesses.length} child processes)`);
    stopping = true;

    // Clear any pending timers
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }

    // Kill all tracked child processes
    if (childProcesses.length > 0) {
      console.log(
        `[apx] Killing ${childProcesses.length} child process(es)...`,
      );
      childProcesses.forEach((proc) => {
        if (proc.pid) {
          killProcess(proc);
        }
      });
      childProcesses = [];
    }

    console.log(`[apx] Stopped`);
  }

  function reset(): void {
    console.log(`[apx] Resetting plugin state`);
    stopping = false;
    timer = null;
    isRunningSteps = false;
    childProcesses = [];
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

      // Ensure directory exists as soon as we know the outDir
      ensureOutDirAndGitignore();
    },

    configureServer(server) {
      // Let Vite handle SIGINT/SIGTERM - we'll clean up via server.close and closeBundle
      // DON'T add signal handlers here as they interfere with Vite's signal handling
      // See: https://github.com/vitejs/vite/issues/11434
      server.httpServer?.once("close", () => {
        console.log(`[apx] Server closing, stopping plugin...`);
        stop();
      });

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

      // Don't trigger updates if stopping
      if (stopping) {
        console.log(`[apx] HMR update ignored (stopping)`);
        return;
      }

      // Check if file should be ignored
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }

      console.log(`[apx] HMR update detected: ${ctx.file}`);

      // Debounce step execution on HMR updates
      if (timer) {
        clearTimeout(timer);
      }

      timer = setTimeout(async () => {
        timer = null;

        // Double-check we're not stopping before running
        if (stopping) {
          return;
        }
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
