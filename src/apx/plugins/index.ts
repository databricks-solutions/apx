import { existsSync, mkdirSync, writeFileSync, readFileSync } from "fs";
import { join, resolve } from "path";
import { type Plugin } from "vite";
import { exec } from "child_process";
import { promisify } from "util";
import { createHash } from "crypto";
import { generate, type OutputOptions } from "orval";

const execAsync = promisify(exec);

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
  action: `uv run apx openapi ${appModule} ${outputPath}`,
});

/**
 * Predefined step for generating API client with Orval
 * Skips generation if the OpenAPI spec hasn't changed since last run
 * @param input - Path to the OpenAPI spec file
 * @param output - Orval output configuration
 */
export const Orval = ({input, output}: {input: string, output: OutputOptions}): StepSpec => ({
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

  async function executeAction(action: StepAction): Promise<void> {
    if (stopping) return;

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
    if (stopping || isRunningSteps) return;

    isRunningSteps = true;

    try {
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
    stopping = true;
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function reset(): void {
    stopping = false;
    timer = null;
    isRunningSteps = false;
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
