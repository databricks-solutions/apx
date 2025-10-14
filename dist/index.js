// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync, readFileSync } from "fs";
import { join, resolve } from "path";
import { exec } from "child_process";
import { promisify } from "util";
import { createHash } from "crypto";
import { generate } from "orval";
function apx(options = {}) {
  const { steps = [], ignore = [] } = options;
  let outDir;
  let timer = null;
  let stopping = false;
  let resolvedIgnores = [];
  let isRunningSteps = false;
  let currentAbortController = null;
  async function executeAction(action) {
    if (stopping)
      return;
    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      currentAbortController = new AbortController;
      try {
        const { stdout, stderr } = await execAsync(action, {
          signal: currentAbortController.signal
        });
        if (stdout)
          console.log(stdout.trim());
        if (stderr)
          console.error(stderr.trim());
      } catch (err) {
        if (err.name === "AbortError" || stopping) {
          console.log(`[apx] Command aborted`);
          return;
        }
        throw err;
      } finally {
        currentAbortController = null;
      }
    } else {
      if (stopping)
        return;
      await action();
    }
    ensureOutDirAndGitignore();
  }
  async function runAllSteps() {
    if (stopping || isRunningSteps)
      return;
    isRunningSteps = true;
    try {
      for (const step of steps) {
        if (stopping)
          break;
        const start = Date.now();
        try {
          console.log(`[apx] ${step.name} \u23F3`);
          await executeAction(step.action);
          console.log(`[apx] ${step.name} \u2713 (${Date.now() - start} ms)`);
        } catch (err) {
          console.error(`[apx] ${step.name} \u2717`, err);
          throw err;
        }
      }
    } finally {
      isRunningSteps = false;
    }
  }
  function ensureOutDirAndGitignore() {
    if (!outDir) {
      console.error(`[apx] outDir is not set`);
      return;
    }
    try {
      if (!existsSync(outDir)) {
        mkdirSync(outDir, { recursive: true });
      }
      const gitignorePath = join(outDir, ".gitignore");
      if (!existsSync(gitignorePath)) {
        writeFileSync(gitignorePath, "*\n");
      }
    } catch (err) {
      console.error(`[apx] failed to ensure output directory:`, err);
    }
  }
  function stop() {
    if (stopping)
      return;
    stopping = true;
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    if (currentAbortController) {
      currentAbortController.abort();
      currentAbortController = null;
    }
  }
  function reset() {
    stopping = false;
    timer = null;
    isRunningSteps = false;
    currentAbortController = null;
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      ensureOutDirAndGitignore();
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
      const signalHandler = () => {
        stop();
      };
      process.once("SIGINT", signalHandler);
      process.once("SIGTERM", signalHandler);
      ensureOutDirAndGitignore();
    },
    async buildStart() {
      ensureOutDirAndGitignore();
      if (steps.length > 0) {
        await runAllSteps();
      }
    },
    handleHotUpdate(ctx) {
      ensureOutDirAndGitignore();
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }
      if (timer)
        clearTimeout(timer);
      timer = setTimeout(async () => {
        timer = null;
        ensureOutDirAndGitignore();
        await runAllSteps();
        ensureOutDirAndGitignore();
      }, 100);
      timer.unref();
    },
    writeBundle() {
      ensureOutDirAndGitignore();
    },
    closeBundle() {
      ensureOutDirAndGitignore();
      stop();
    }
  };
}
var execAsync = promisify(exec);
var specHashCache = new Map;
var Step = (spec) => spec;
var OpenAPI = (appModule, outputPath) => ({
  name: "openapi",
  action: `uv run apx openapi ${appModule} ${outputPath}`
});
var Orval = ({ input, output }) => ({
  name: "orval",
  action: async () => {
    if (!existsSync(input)) {
      console.warn(`[apx] OpenAPI spec not found at ${input}, skipping Orval generation`);
      return;
    }
    const specContent = readFileSync(input, "utf-8");
    const specHash = createHash("sha256").update(specContent).digest("hex");
    const cachedHash = specHashCache.get(input);
    if (cachedHash === specHash) {
      console.log(`[apx] OpenAPI spec unchanged, skipping Orval generation`);
      return;
    }
    await generate({
      input,
      output
    });
    specHashCache.set(input, specHash);
  }
});
var plugins_default = apx;
export {
  plugins_default as default,
  apx,
  Step,
  Orval,
  OpenAPI
};

//# debugId=82F309A0703E28AA64756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYywgcmVhZEZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luLCByZXNvbHZlIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IGV4ZWMgfSBmcm9tIFwiY2hpbGRfcHJvY2Vzc1wiO1xuaW1wb3J0IHsgcHJvbWlzaWZ5IH0gZnJvbSBcInV0aWxcIjtcbmltcG9ydCB7IGNyZWF0ZUhhc2ggfSBmcm9tIFwiY3J5cHRvXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPdXRwdXRPcHRpb25zIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gQ2FjaGUgZm9yIE9wZW5BUEkgc3BlYyBoYXNoZXMgdG8gZGV0ZWN0IGNoYW5nZXNcbmNvbnN0IHNwZWNIYXNoQ2FjaGUgPSBuZXcgTWFwPHN0cmluZywgc3RyaW5nPigpO1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBTa2lwcyBnZW5lcmF0aW9uIGlmIHRoZSBPcGVuQVBJIHNwZWMgaGFzbid0IGNoYW5nZWQgc2luY2UgbGFzdCBydW5cbiAqIEBwYXJhbSBpbnB1dCAtIFBhdGggdG8gdGhlIE9wZW5BUEkgc3BlYyBmaWxlXG4gKiBAcGFyYW0gb3V0cHV0IC0gT3J2YWwgb3V0cHV0IGNvbmZpZ3VyYXRpb25cbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKHtpbnB1dCwgb3V0cHV0fToge2lucHV0OiBzdHJpbmcsIG91dHB1dDogT3V0cHV0T3B0aW9uc30pOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogYXN5bmMgKCkgPT4ge1xuICAgIC8vIENoZWNrIGlmIHNwZWMgZmlsZSBleGlzdHNcbiAgICBpZiAoIWV4aXN0c1N5bmMoaW5wdXQpKSB7XG4gICAgICBjb25zb2xlLndhcm4oXG4gICAgICAgIGBbYXB4XSBPcGVuQVBJIHNwZWMgbm90IGZvdW5kIGF0ICR7aW5wdXR9LCBza2lwcGluZyBPcnZhbCBnZW5lcmF0aW9uYCxcbiAgICAgICk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgLy8gUmVhZCBhbmQgaGFzaCB0aGUgc3BlYyBmaWxlXG4gICAgY29uc3Qgc3BlY0NvbnRlbnQgPSByZWFkRmlsZVN5bmMoaW5wdXQsIFwidXRmLThcIik7XG4gICAgY29uc3Qgc3BlY0hhc2ggPSBjcmVhdGVIYXNoKFwic2hhMjU2XCIpLnVwZGF0ZShzcGVjQ29udGVudCkuZGlnZXN0KFwiaGV4XCIpO1xuXG4gICAgLy8gQ2hlY2sgaWYgc3BlYyBoYXMgY2hhbmdlZFxuICAgIGNvbnN0IGNhY2hlZEhhc2ggPSBzcGVjSGFzaENhY2hlLmdldChpbnB1dCk7XG4gICAgaWYgKGNhY2hlZEhhc2ggPT09IHNwZWNIYXNoKSB7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gT3BlbkFQSSBzcGVjIHVuY2hhbmdlZCwgc2tpcHBpbmcgT3J2YWwgZ2VuZXJhdGlvbmApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIC8vIEdlbmVyYXRlIEFQSSBjbGllbnRcbiAgICBhd2FpdCBnZW5lcmF0ZSh7XG4gICAgICBpbnB1dCxcbiAgICAgIG91dHB1dCxcbiAgICB9KTtcblxuICAgIC8vIFVwZGF0ZSBjYWNoZVxuICAgIHNwZWNIYXNoQ2FjaGUuc2V0KGlucHV0LCBzcGVjSGFzaCk7XG4gIH0sXG59KTtcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgbGV0IGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG4gIGxldCBjdXJyZW50QWJvcnRDb250cm9sbGVyOiBBYm9ydENvbnRyb2xsZXIgfCBudWxsID0gbnVsbDtcblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZykgcmV0dXJuO1xuXG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgaWYgKHR5cGVvZiBhY3Rpb24gPT09IFwic3RyaW5nXCIpIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgc2hlbGwgY29tbWFuZCB3aXRoIGFib3J0IHN1cHBvcnRcbiAgICAgIGN1cnJlbnRBYm9ydENvbnRyb2xsZXIgPSBuZXcgQWJvcnRDb250cm9sbGVyKCk7XG4gICAgICB0cnkge1xuICAgICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uLCB7XG4gICAgICAgICAgc2lnbmFsOiBjdXJyZW50QWJvcnRDb250cm9sbGVyLnNpZ25hbCxcbiAgICAgICAgfSk7XG4gICAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgICBpZiAoc3RkZXJyKSBjb25zb2xlLmVycm9yKHN0ZGVyci50cmltKCkpO1xuICAgICAgfSBjYXRjaCAoZXJyOiBhbnkpIHtcbiAgICAgICAgaWYgKGVyci5uYW1lID09PSAnQWJvcnRFcnJvcicgfHwgc3RvcHBpbmcpIHtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gQ29tbWFuZCBhYm9ydGVkYCk7XG4gICAgICAgICAgcmV0dXJuO1xuICAgICAgICB9XG4gICAgICAgIHRocm93IGVycjtcbiAgICAgIH0gZmluYWxseSB7XG4gICAgICAgIGN1cnJlbnRBYm9ydENvbnRyb2xsZXIgPSBudWxsO1xuICAgICAgfVxuICAgIH0gZWxzZSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIGZ1bmN0aW9uXG4gICAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICAgIGF3YWl0IGFjdGlvbigpO1xuICAgIH1cbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgfVxuXG4gIGFzeW5jIGZ1bmN0aW9uIHJ1bkFsbFN0ZXBzKCk6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZyB8fCBpc1J1bm5pbmdTdGVwcykgcmV0dXJuO1xuXG4gICAgaXNSdW5uaW5nU3RlcHMgPSB0cnVlO1xuXG4gICAgdHJ5IHtcbiAgICAgIGZvciAoY29uc3Qgc3RlcCBvZiBzdGVwcykge1xuICAgICAgICBpZiAoc3RvcHBpbmcpIGJyZWFrO1xuICAgICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICAgIHRyeSB7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgICBhd2FpdCBleGVjdXRlQWN0aW9uKHN0ZXAuYWN0aW9uKTtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKckyAoJHtEYXRlLm5vdygpIC0gc3RhcnR9IG1zKWApO1xuICAgICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyXYCwgZXJyKTtcbiAgICAgICAgICB0aHJvdyBlcnI7XG4gICAgICAgIH1cbiAgICAgIH1cbiAgICB9IGZpbmFsbHkge1xuICAgICAgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcbiAgICB9XG4gIH1cblxuICAvKipcbiAgICogRW5zdXJlcyB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHMgYW5kIGNvbnRhaW5zIGEgLmdpdGlnbm9yZSBmaWxlLlxuICAgKiBUaGlzIGlzIGNhbGxlZCBhdCBtdWx0aXBsZSBwb2ludHMgdG8gZ3VhcmFudGVlIHRoZSBkaXJlY3RvcnkgaXMgYWx3YXlzIHByZXNlbnQuXG4gICAqL1xuICBmdW5jdGlvbiBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIG91dERpciBpcyBub3Qgc2V0YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgdHJ5IHtcbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzXG4gICAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICAgIH1cblxuICAgICAgLy8gQWx3YXlzIGVuc3VyZSAuZ2l0aWdub3JlIGV4aXN0cyBpbiBvdXRwdXQgZGlyZWN0b3J5XG4gICAgICBjb25zdCBnaXRpZ25vcmVQYXRoID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgICB3cml0ZUZpbGVTeW5jKGdpdGlnbm9yZVBhdGgsIFwiKlxcblwiKTtcbiAgICAgIH1cbiAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIGZhaWxlZCB0byBlbnN1cmUgb3V0cHV0IGRpcmVjdG9yeTpgLCBlcnIpO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikge1xuICAgICAgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gbnVsbDtcbiAgICB9XG4gICAgLy8gQWJvcnQgYW55IHJ1bm5pbmcgc2hlbGwgY29tbWFuZHNcbiAgICBpZiAoY3VycmVudEFib3J0Q29udHJvbGxlcikge1xuICAgICAgY3VycmVudEFib3J0Q29udHJvbGxlci5hYm9ydCgpO1xuICAgICAgY3VycmVudEFib3J0Q29udHJvbGxlciA9IG51bGw7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gICAgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcbiAgICBjdXJyZW50QWJvcnRDb250cm9sbGVyID0gbnVsbDtcbiAgfVxuXG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJhcHhcIixcbiAgICBhcHBseTogKCkgPT4gdHJ1ZSxcblxuICAgIGNvbmZpZ1Jlc29sdmVkKGNvbmZpZykge1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuXG4gICAgICAvLyBIYW5kbGUgcHJvY2VzcyBzaWduYWxzIGZvciBjbGVhbiBzaHV0ZG93blxuICAgICAgY29uc3Qgc2lnbmFsSGFuZGxlciA9ICgpID0+IHtcbiAgICAgICAgc3RvcCgpO1xuICAgICAgfTtcbiAgICAgIHByb2Nlc3Mub25jZShcIlNJR0lOVFwiLCBzaWduYWxIYW5kbGVyKTtcbiAgICAgIHByb2Nlc3Mub25jZShcIlNJR1RFUk1cIiwgc2lnbmFsSGFuZGxlcik7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIHdoZW4gc2VydmVyIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGFzeW5jIGJ1aWxkU3RhcnQoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgYnVpbGQgc3RhcnRzXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb24gZXZlcnkgSE1SIHVwZGF0ZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIC8vIENoZWNrIGlmIGZpbGUgc2hvdWxkIGJlIGlnbm9yZWRcbiAgICAgIGlmIChyZXNvbHZlZElnbm9yZXMuc29tZSgocGF0dGVybikgPT4gY3R4LmZpbGUuaW5jbHVkZXMocGF0dGVybikpKSB7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgLy8gRGVib3VuY2Ugc3RlcCBleGVjdXRpb24gb24gSE1SIHVwZGF0ZXNcbiAgICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcblxuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KGFzeW5jICgpID0+IHtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGJlZm9yZSBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgICBhd2FpdCBydW5BbGxTdGVwcygpO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICB9LCAxMDApO1xuXG4gICAgICAvLyBBbGxvdyB0aGUgcHJvY2VzcyB0byBleGl0IGV2ZW4gaWYgdGhpcyB0aW1lciBpcyBwZW5kaW5nXG4gICAgICB0aW1lci51bnJlZigpO1xuICAgIH0sXG5cbiAgICB3cml0ZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIGZpbGVzIGFyZSB3cml0dGVuXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICB9LFxuXG4gICAgY2xvc2VCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBvbmUgZmluYWwgdGltZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICBzdG9wKCk7XG4gICAgfSxcbiAgfTtcbn1cblxuLy8gRGVmYXVsdCBleHBvcnQgZm9yIGNvbnZlbmllbmNlOiBpbXBvcnQgYXB4IGZyb20gXCJhcHhcIlxuZXhwb3J0IGRlZmF1bHQgYXB4O1xuIgogIF0sCiAgIm1hcHBpbmdzIjogIjtBQUFBO0FBQ0E7QUFFQTtBQUNBO0FBQ0E7QUFDQTtBQXNFTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFTLENBQUMsTUFBTTtBQUVwQyxNQUFJO0FBQ0osTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUE0QixDQUFDO0FBQ2pDLE1BQUksaUJBQWlCO0FBQ3JCLE1BQUkseUJBQWlEO0FBRXJELGlCQUFlLGFBQWEsQ0FBQyxRQUFtQztBQUM5RCxRQUFJO0FBQVU7QUFFZCw2QkFBeUI7QUFDekIsZUFBVyxXQUFXLFVBQVU7QUFFOUIsK0JBQXlCLElBQUk7QUFDN0IsVUFBSTtBQUNGLGdCQUFRLFFBQVEsV0FBVyxNQUFNLFVBQVUsUUFBUTtBQUFBLFVBQ2pELFFBQVEsdUJBQXVCO0FBQUEsUUFDakMsQ0FBQztBQUNELFlBQUk7QUFBUSxrQkFBUSxJQUFJLE9BQU8sS0FBSyxDQUFDO0FBQ3JDLFlBQUk7QUFBUSxrQkFBUSxNQUFNLE9BQU8sS0FBSyxDQUFDO0FBQUEsZUFDaEMsS0FBUDtBQUNBLFlBQUksSUFBSSxTQUFTLGdCQUFnQixVQUFVO0FBQ3pDLGtCQUFRLElBQUksdUJBQXVCO0FBQ25DO0FBQUEsUUFDRjtBQUNBLGNBQU07QUFBQSxnQkFDTjtBQUNBLGlDQUF5QjtBQUFBO0FBQUEsSUFFN0IsT0FBTztBQUVMLFVBQUk7QUFBVTtBQUNkLFlBQU0sT0FBTztBQUFBO0FBRWYsNkJBQXlCO0FBQUE7QUFHM0IsaUJBQWUsV0FBVyxHQUFrQjtBQUMxQyxRQUFJLFlBQVk7QUFBZ0I7QUFFaEMscUJBQWlCO0FBRWpCLFFBQUk7QUFDRixpQkFBVyxRQUFRLE9BQU87QUFDeEIsWUFBSTtBQUFVO0FBQ2QsY0FBTSxRQUFRLEtBQUssSUFBSTtBQUN2QixZQUFJO0FBQ0Ysa0JBQVEsSUFBSSxTQUFTLEtBQUssYUFBTztBQUNqQyxnQkFBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixrQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsaUJBQ3JELEtBQVA7QUFDQSxrQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsZ0JBQU07QUFBQTtBQUFBLE1BRVY7QUFBQSxjQUNBO0FBQ0EsdUJBQWlCO0FBQUE7QUFBQTtBQVFyQixXQUFTLHdCQUF3QixHQUFTO0FBQ3hDLFNBQUssUUFBUTtBQUNYLGNBQVEsTUFBTSx5QkFBeUI7QUFDdkM7QUFBQSxJQUNGO0FBRUEsUUFBSTtBQUVGLFdBQUssV0FBVyxNQUFNLEdBQUc7QUFDdkIsa0JBQVUsUUFBUSxFQUFFLFdBQVcsS0FBSyxDQUFDO0FBQUEsTUFDdkM7QUFHQSxZQUFNLGdCQUFnQixLQUFLLFFBQVEsWUFBWTtBQUMvQyxXQUFLLFdBQVcsYUFBYSxHQUFHO0FBQzlCLHNCQUFjLGVBQWUsS0FBSztBQUFBLE1BQ3BDO0FBQUEsYUFDTyxLQUFQO0FBQ0EsY0FBUSxNQUFNLDRDQUE0QyxHQUFHO0FBQUE7QUFBQTtBQUlqRSxXQUFTLElBQUksR0FBUztBQUNwQixRQUFJO0FBQVU7QUFDZCxlQUFXO0FBQ1gsUUFBSSxPQUFPO0FBQ1QsbUJBQWEsS0FBSztBQUNsQixjQUFRO0FBQUEsSUFDVjtBQUVBLFFBQUksd0JBQXdCO0FBQzFCLDZCQUF1QixNQUFNO0FBQzdCLCtCQUF5QjtBQUFBLElBQzNCO0FBQUE7QUFHRixXQUFTLEtBQUssR0FBUztBQUNyQixlQUFXO0FBQ1gsWUFBUTtBQUNSLHFCQUFpQjtBQUNqQiw2QkFBeUI7QUFBQTtBQUczQixTQUFPO0FBQUEsSUFDTCxNQUFNO0FBQUEsSUFDTixPQUFPLE1BQU07QUFBQSxJQUViLGNBQWMsQ0FBQyxRQUFRO0FBQ3JCLGVBQVMsUUFBUSxPQUFPLE1BQU0sT0FBTyxNQUFNLE1BQU07QUFDakQsd0JBQWtCLE9BQU8sSUFBSSxDQUFDLFlBQzVCLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUNoQztBQUdBLFlBQU07QUFHTiwrQkFBeUI7QUFBQTtBQUFBLElBRzNCLGVBQWUsQ0FBQyxRQUFRO0FBQ3RCLGFBQU8sWUFBWSxLQUFLLFNBQVMsSUFBSTtBQUdyQyxZQUFNLGdCQUFnQixNQUFNO0FBQzFCLGFBQUs7QUFBQTtBQUVQLGNBQVEsS0FBSyxVQUFVLGFBQWE7QUFDcEMsY0FBUSxLQUFLLFdBQVcsYUFBYTtBQUdyQywrQkFBeUI7QUFBQTtBQUFBLFNBR3JCLFdBQVUsR0FBRztBQUVqQiwrQkFBeUI7QUFFekIsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQiwrQkFBeUI7QUFHekIsVUFBSSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBR0EsVUFBSTtBQUFPLHFCQUFhLEtBQUs7QUFFN0IsY0FBUSxXQUFXLFlBQVk7QUFDN0IsZ0JBQVE7QUFHUixpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFHTixZQUFNLE1BQU07QUFBQTtBQUFBLElBR2QsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBL1BGLElBQU0sWUFBWSxVQUFVLElBQUk7QUFHaEMsSUFBTSxnQkFBZ0IsSUFBSTtBQVNuQixJQUFNLE9BQU8sQ0FBQyxTQUE2QjtBQU8zQyxJQUFNLFVBQVUsQ0FBQyxXQUFtQixnQkFBa0M7QUFBQSxFQUMzRSxNQUFNO0FBQUEsRUFDTixRQUFRLHNCQUFzQixhQUFhO0FBQzdDO0FBUU8sSUFBTSxRQUFRLEdBQUUsT0FBTyxjQUErRDtBQUFBLEVBQzNGLE1BQU07QUFBQSxFQUNOLFFBQVEsWUFBWTtBQUVsQixTQUFLLFdBQVcsS0FBSyxHQUFHO0FBQ3RCLGNBQVEsS0FDTixtQ0FBbUMsa0NBQ3JDO0FBQ0E7QUFBQSxJQUNGO0FBR0EsVUFBTSxjQUFjLGFBQWEsT0FBTyxPQUFPO0FBQy9DLFVBQU0sV0FBVyxXQUFXLFFBQVEsRUFBRSxPQUFPLFdBQVcsRUFBRSxPQUFPLEtBQUs7QUFHdEUsVUFBTSxhQUFhLGNBQWMsSUFBSSxLQUFLO0FBQzFDLFFBQUksZUFBZSxVQUFVO0FBQzNCLGNBQVEsSUFBSSx5REFBeUQ7QUFDckU7QUFBQSxJQUNGO0FBR0EsVUFBTSxTQUFTO0FBQUEsTUFDYjtBQUFBLE1BQ0E7QUFBQSxJQUNGLENBQUM7QUFHRCxrQkFBYyxJQUFJLE9BQU8sUUFBUTtBQUFBO0FBRXJDO0FBc01BLElBQWU7IiwKICAiZGVidWdJZCI6ICI4MkYzMDlBMDcwM0UyOEFBNjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
