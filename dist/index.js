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
  async function executeAction(action) {
    if (stopping)
      return;
    ensureOutDirAndGitignore();
    if (typeof action === "string") {
      const { stdout, stderr } = await execAsync(action);
      if (stdout)
        console.log(stdout.trim());
      if (stderr)
        console.error(stderr.trim());
    } else {
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
  }
  function reset() {
    stopping = false;
    timer = null;
    isRunningSteps = false;
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

//# debugId=4D09D2726AF4BE5964756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYywgcmVhZEZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luLCByZXNvbHZlIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IGV4ZWMgfSBmcm9tIFwiY2hpbGRfcHJvY2Vzc1wiO1xuaW1wb3J0IHsgcHJvbWlzaWZ5IH0gZnJvbSBcInV0aWxcIjtcbmltcG9ydCB7IGNyZWF0ZUhhc2ggfSBmcm9tIFwiY3J5cHRvXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPdXRwdXRPcHRpb25zIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gQ2FjaGUgZm9yIE9wZW5BUEkgc3BlYyBoYXNoZXMgdG8gZGV0ZWN0IGNoYW5nZXNcbmNvbnN0IHNwZWNIYXNoQ2FjaGUgPSBuZXcgTWFwPHN0cmluZywgc3RyaW5nPigpO1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBTa2lwcyBnZW5lcmF0aW9uIGlmIHRoZSBPcGVuQVBJIHNwZWMgaGFzbid0IGNoYW5nZWQgc2luY2UgbGFzdCBydW5cbiAqIEBwYXJhbSBpbnB1dCAtIFBhdGggdG8gdGhlIE9wZW5BUEkgc3BlYyBmaWxlXG4gKiBAcGFyYW0gb3V0cHV0IC0gT3J2YWwgb3V0cHV0IGNvbmZpZ3VyYXRpb25cbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKHtpbnB1dCwgb3V0cHV0fToge2lucHV0OiBzdHJpbmcsIG91dHB1dDogT3V0cHV0T3B0aW9uc30pOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogYXN5bmMgKCkgPT4ge1xuICAgIC8vIENoZWNrIGlmIHNwZWMgZmlsZSBleGlzdHNcbiAgICBpZiAoIWV4aXN0c1N5bmMoaW5wdXQpKSB7XG4gICAgICBjb25zb2xlLndhcm4oXG4gICAgICAgIGBbYXB4XSBPcGVuQVBJIHNwZWMgbm90IGZvdW5kIGF0ICR7aW5wdXR9LCBza2lwcGluZyBPcnZhbCBnZW5lcmF0aW9uYCxcbiAgICAgICk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgLy8gUmVhZCBhbmQgaGFzaCB0aGUgc3BlYyBmaWxlXG4gICAgY29uc3Qgc3BlY0NvbnRlbnQgPSByZWFkRmlsZVN5bmMoaW5wdXQsIFwidXRmLThcIik7XG4gICAgY29uc3Qgc3BlY0hhc2ggPSBjcmVhdGVIYXNoKFwic2hhMjU2XCIpLnVwZGF0ZShzcGVjQ29udGVudCkuZGlnZXN0KFwiaGV4XCIpO1xuXG4gICAgLy8gQ2hlY2sgaWYgc3BlYyBoYXMgY2hhbmdlZFxuICAgIGNvbnN0IGNhY2hlZEhhc2ggPSBzcGVjSGFzaENhY2hlLmdldChpbnB1dCk7XG4gICAgaWYgKGNhY2hlZEhhc2ggPT09IHNwZWNIYXNoKSB7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gT3BlbkFQSSBzcGVjIHVuY2hhbmdlZCwgc2tpcHBpbmcgT3J2YWwgZ2VuZXJhdGlvbmApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIC8vIEdlbmVyYXRlIEFQSSBjbGllbnRcbiAgICBhd2FpdCBnZW5lcmF0ZSh7XG4gICAgICBpbnB1dCxcbiAgICAgIG91dHB1dCxcbiAgICB9KTtcblxuICAgIC8vIFVwZGF0ZSBjYWNoZVxuICAgIHNwZWNIYXNoQ2FjaGUuc2V0KGlucHV0LCBzcGVjSGFzaCk7XG4gIH0sXG59KTtcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgbGV0IGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG5cbiAgYXN5bmMgZnVuY3Rpb24gZXhlY3V0ZUFjdGlvbihhY3Rpb246IFN0ZXBBY3Rpb24pOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcblxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIGlmICh0eXBlb2YgYWN0aW9uID09PSBcInN0cmluZ1wiKSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIHNoZWxsIGNvbW1hbmRcbiAgICAgIGNvbnN0IHsgc3Rkb3V0LCBzdGRlcnIgfSA9IGF3YWl0IGV4ZWNBc3luYyhhY3Rpb24pO1xuICAgICAgaWYgKHN0ZG91dCkgY29uc29sZS5sb2coc3Rkb3V0LnRyaW0oKSk7XG4gICAgICBpZiAoc3RkZXJyKSBjb25zb2xlLmVycm9yKHN0ZGVyci50cmltKCkpO1xuICAgIH0gZWxzZSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIGZ1bmN0aW9uXG4gICAgICBhd2FpdCBhY3Rpb24oKTtcbiAgICB9XG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gIH1cblxuICBhc3luYyBmdW5jdGlvbiBydW5BbGxTdGVwcygpOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcgfHwgaXNSdW5uaW5nU3RlcHMpIHJldHVybjtcblxuICAgIGlzUnVubmluZ1N0ZXBzID0gdHJ1ZTtcblxuICAgIHRyeSB7XG4gICAgICBmb3IgKGNvbnN0IHN0ZXAgb2Ygc3RlcHMpIHtcbiAgICAgICAgaWYgKHN0b3BwaW5nKSBicmVhaztcbiAgICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgICB0cnkge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4o+zYCk7XG4gICAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDinJMgKCR7RGF0ZS5ub3coKSAtIHN0YXJ0fSBtcylgKTtcbiAgICAgICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgICAgdGhyb3cgZXJyO1xuICAgICAgICB9XG4gICAgICB9XG4gICAgfSBmaW5hbGx5IHtcbiAgICAgIGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG4gICAgfVxuICB9XG5cbiAgLyoqXG4gICAqIEVuc3VyZXMgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzIGFuZCBjb250YWlucyBhIC5naXRpZ25vcmUgZmlsZS5cbiAgICogVGhpcyBpcyBjYWxsZWQgYXQgbXVsdGlwbGUgcG9pbnRzIHRvIGd1YXJhbnRlZSB0aGUgZGlyZWN0b3J5IGlzIGFsd2F5cyBwcmVzZW50LlxuICAgKi9cbiAgZnVuY3Rpb24gZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk6IHZvaWQge1xuICAgIGlmICghb3V0RGlyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBvdXREaXIgaXMgbm90IHNldGApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIHRyeSB7XG4gICAgICAvLyBBbHdheXMgZW5zdXJlIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGV4aXN0c1xuICAgICAgaWYgKCFleGlzdHNTeW5jKG91dERpcikpIHtcbiAgICAgICAgbWtkaXJTeW5jKG91dERpciwgeyByZWN1cnNpdmU6IHRydWUgfSk7XG4gICAgICB9XG5cbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgICAgY29uc3QgZ2l0aWdub3JlUGF0aCA9IGpvaW4ob3V0RGlyLCBcIi5naXRpZ25vcmVcIik7XG4gICAgICBpZiAoIWV4aXN0c1N5bmMoZ2l0aWdub3JlUGF0aCkpIHtcbiAgICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICB9XG4gICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBmYWlsZWQgdG8gZW5zdXJlIG91dHB1dCBkaXJlY3Rvcnk6YCwgZXJyKTtcbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiBzdG9wKCk6IHZvaWQge1xuICAgIGlmIChzdG9wcGluZykgcmV0dXJuO1xuICAgIHN0b3BwaW5nID0gdHJ1ZTtcbiAgICBpZiAodGltZXIpIHtcbiAgICAgIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB0aW1lciA9IG51bGw7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gICAgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcbiAgfVxuXG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJhcHhcIixcbiAgICBhcHBseTogKCkgPT4gdHJ1ZSxcblxuICAgIGNvbmZpZ1Jlc29sdmVkKGNvbmZpZykge1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyB3aGVuIHNlcnZlciBzdGFydHNcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIGJ1aWxkIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uIGV2ZXJ5IEhNUiB1cGRhdGVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuXG4gICAgICAvLyBDaGVjayBpZiBmaWxlIHNob3VsZCBiZSBpZ25vcmVkXG4gICAgICBpZiAocmVzb2x2ZWRJZ25vcmVzLnNvbWUoKHBhdHRlcm4pID0+IGN0eC5maWxlLmluY2x1ZGVzKHBhdHRlcm4pKSkge1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIC8vIERlYm91bmNlIHN0ZXAgZXhlY3V0aW9uIG9uIEhNUiB1cGRhdGVzXG4gICAgICBpZiAodGltZXIpIGNsZWFyVGltZW91dCh0aW1lcik7XG5cbiAgICAgIHRpbWVyID0gc2V0VGltZW91dChhc3luYyAoKSA9PiB7XG4gICAgICAgIHRpbWVyID0gbnVsbDtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgfSwgMTAwKTtcblxuICAgICAgLy8gQWxsb3cgdGhlIHByb2Nlc3MgdG8gZXhpdCBldmVuIGlmIHRoaXMgdGltZXIgaXMgcGVuZGluZ1xuICAgICAgdGltZXIudW5yZWYoKTtcbiAgICB9LFxuXG4gICAgd3JpdGVCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBmaWxlcyBhcmUgd3JpdHRlblxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb25lIGZpbmFsIHRpbWVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDtcbiIKICBdLAogICJtYXBwaW5ncyI6ICI7QUFBQTtBQUNBO0FBRUE7QUFDQTtBQUNBO0FBQ0E7QUFzRU8sU0FBUyxHQUFHLENBQUMsVUFBNEIsQ0FBQyxHQUFXO0FBQzFELFVBQVEsUUFBUSxDQUFDLEdBQUcsU0FBUyxDQUFDLE1BQU07QUFFcEMsTUFBSTtBQUNKLE1BQUksUUFBK0I7QUFDbkMsTUFBSSxXQUFXO0FBQ2YsTUFBSSxrQkFBNEIsQ0FBQztBQUNqQyxNQUFJLGlCQUFpQjtBQUVyQixpQkFBZSxhQUFhLENBQUMsUUFBbUM7QUFDOUQsUUFBSTtBQUFVO0FBRWQsNkJBQXlCO0FBQ3pCLGVBQVcsV0FBVyxVQUFVO0FBRTlCLGNBQVEsUUFBUSxXQUFXLE1BQU0sVUFBVSxNQUFNO0FBQ2pELFVBQUk7QUFBUSxnQkFBUSxJQUFJLE9BQU8sS0FBSyxDQUFDO0FBQ3JDLFVBQUk7QUFBUSxnQkFBUSxNQUFNLE9BQU8sS0FBSyxDQUFDO0FBQUEsSUFDekMsT0FBTztBQUVMLFlBQU0sT0FBTztBQUFBO0FBRWYsNkJBQXlCO0FBQUE7QUFHM0IsaUJBQWUsV0FBVyxHQUFrQjtBQUMxQyxRQUFJLFlBQVk7QUFBZ0I7QUFFaEMscUJBQWlCO0FBRWpCLFFBQUk7QUFDRixpQkFBVyxRQUFRLE9BQU87QUFDeEIsWUFBSTtBQUFVO0FBQ2QsY0FBTSxRQUFRLEtBQUssSUFBSTtBQUN2QixZQUFJO0FBQ0Ysa0JBQVEsSUFBSSxTQUFTLEtBQUssYUFBTztBQUNqQyxnQkFBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixrQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsaUJBQ3JELEtBQVA7QUFDQSxrQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsZ0JBQU07QUFBQTtBQUFBLE1BRVY7QUFBQSxjQUNBO0FBQ0EsdUJBQWlCO0FBQUE7QUFBQTtBQVFyQixXQUFTLHdCQUF3QixHQUFTO0FBQ3hDLFNBQUssUUFBUTtBQUNYLGNBQVEsTUFBTSx5QkFBeUI7QUFDdkM7QUFBQSxJQUNGO0FBRUEsUUFBSTtBQUVGLFdBQUssV0FBVyxNQUFNLEdBQUc7QUFDdkIsa0JBQVUsUUFBUSxFQUFFLFdBQVcsS0FBSyxDQUFDO0FBQUEsTUFDdkM7QUFHQSxZQUFNLGdCQUFnQixLQUFLLFFBQVEsWUFBWTtBQUMvQyxXQUFLLFdBQVcsYUFBYSxHQUFHO0FBQzlCLHNCQUFjLGVBQWUsS0FBSztBQUFBLE1BQ3BDO0FBQUEsYUFDTyxLQUFQO0FBQ0EsY0FBUSxNQUFNLDRDQUE0QyxHQUFHO0FBQUE7QUFBQTtBQUlqRSxXQUFTLElBQUksR0FBUztBQUNwQixRQUFJO0FBQVU7QUFDZCxlQUFXO0FBQ1gsUUFBSSxPQUFPO0FBQ1QsbUJBQWEsS0FBSztBQUNsQixjQUFRO0FBQUEsSUFDVjtBQUFBO0FBR0YsV0FBUyxLQUFLLEdBQVM7QUFDckIsZUFBVztBQUNYLFlBQVE7QUFDUixxQkFBaUI7QUFBQTtBQUduQixTQUFPO0FBQUEsSUFDTCxNQUFNO0FBQUEsSUFDTixPQUFPLE1BQU07QUFBQSxJQUViLGNBQWMsQ0FBQyxRQUFRO0FBQ3JCLGVBQVMsUUFBUSxPQUFPLE1BQU0sT0FBTyxNQUFNLE1BQU07QUFDakQsd0JBQWtCLE9BQU8sSUFBSSxDQUFDLFlBQzVCLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUNoQztBQUdBLFlBQU07QUFHTiwrQkFBeUI7QUFBQTtBQUFBLElBRzNCLGVBQWUsQ0FBQyxRQUFRO0FBQ3RCLGFBQU8sWUFBWSxLQUFLLFNBQVMsSUFBSTtBQUdyQywrQkFBeUI7QUFBQTtBQUFBLFNBR3JCLFdBQVUsR0FBRztBQUVqQiwrQkFBeUI7QUFFekIsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQiwrQkFBeUI7QUFHekIsVUFBSSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBR0EsVUFBSTtBQUFPLHFCQUFhLEtBQUs7QUFFN0IsY0FBUSxXQUFXLFlBQVk7QUFDN0IsZ0JBQVE7QUFHUixpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFHTixZQUFNLE1BQU07QUFBQTtBQUFBLElBR2QsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBbk9GLElBQU0sWUFBWSxVQUFVLElBQUk7QUFHaEMsSUFBTSxnQkFBZ0IsSUFBSTtBQVNuQixJQUFNLE9BQU8sQ0FBQyxTQUE2QjtBQU8zQyxJQUFNLFVBQVUsQ0FBQyxXQUFtQixnQkFBa0M7QUFBQSxFQUMzRSxNQUFNO0FBQUEsRUFDTixRQUFRLHNCQUFzQixhQUFhO0FBQzdDO0FBUU8sSUFBTSxRQUFRLEdBQUUsT0FBTyxjQUErRDtBQUFBLEVBQzNGLE1BQU07QUFBQSxFQUNOLFFBQVEsWUFBWTtBQUVsQixTQUFLLFdBQVcsS0FBSyxHQUFHO0FBQ3RCLGNBQVEsS0FDTixtQ0FBbUMsa0NBQ3JDO0FBQ0E7QUFBQSxJQUNGO0FBR0EsVUFBTSxjQUFjLGFBQWEsT0FBTyxPQUFPO0FBQy9DLFVBQU0sV0FBVyxXQUFXLFFBQVEsRUFBRSxPQUFPLFdBQVcsRUFBRSxPQUFPLEtBQUs7QUFHdEUsVUFBTSxhQUFhLGNBQWMsSUFBSSxLQUFLO0FBQzFDLFFBQUksZUFBZSxVQUFVO0FBQzNCLGNBQVEsSUFBSSx5REFBeUQ7QUFDckU7QUFBQSxJQUNGO0FBR0EsVUFBTSxTQUFTO0FBQUEsTUFDYjtBQUFBLE1BQ0E7QUFBQSxJQUNGLENBQUM7QUFHRCxrQkFBYyxJQUFJLE9BQU8sUUFBUTtBQUFBO0FBRXJDO0FBMEtBLElBQWU7IiwKICAiZGVidWdJZCI6ICI0RDA5RDI3MjZBRjRCRTU5NjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
