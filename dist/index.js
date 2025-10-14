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
var Orval = (input, output) => ({
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

//# debugId=AFFAFDAEEB51EB1364756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYywgcmVhZEZpbGVTeW5jIH0gZnJvbSBcImZzXCI7XG5pbXBvcnQgeyBqb2luLCByZXNvbHZlIH0gZnJvbSBcInBhdGhcIjtcbmltcG9ydCB7IHR5cGUgUGx1Z2luIH0gZnJvbSBcInZpdGVcIjtcbmltcG9ydCB7IGV4ZWMgfSBmcm9tIFwiY2hpbGRfcHJvY2Vzc1wiO1xuaW1wb3J0IHsgcHJvbWlzaWZ5IH0gZnJvbSBcInV0aWxcIjtcbmltcG9ydCB7IGNyZWF0ZUhhc2ggfSBmcm9tIFwiY3J5cHRvXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSB9IGZyb20gXCJvcnZhbFwiO1xuXG5jb25zdCBleGVjQXN5bmMgPSBwcm9taXNpZnkoZXhlYyk7XG5cbi8vIENhY2hlIGZvciBPcGVuQVBJIHNwZWMgaGFzaGVzIHRvIGRldGVjdCBjaGFuZ2VzXG5jb25zdCBzcGVjSGFzaENhY2hlID0gbmV3IE1hcDxzdHJpbmcsIHN0cmluZz4oKTtcblxuZXhwb3J0IHR5cGUgU3RlcEFjdGlvbiA9IHN0cmluZyB8ICgoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPik7XG5cbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0ge1xuICBuYW1lOiBzdHJpbmc7XG4gIGFjdGlvbjogU3RlcEFjdGlvbjtcbn07XG5cbmV4cG9ydCBjb25zdCBTdGVwID0gKHNwZWM6IFN0ZXBTcGVjKTogU3RlcFNwZWMgPT4gc3BlYztcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgT3BlbkFQSSBzY2hlbWFcbiAqIEBwYXJhbSBhcHBNb2R1bGUgLSBUaGUgUHl0aG9uIG1vZHVsZSBwYXRoIChlLmcuLCBcInNhbXBsZS5hcGkuYXBwOmFwcFwiKVxuICogQHBhcmFtIG91dHB1dFBhdGggLSBXaGVyZSB0byB3cml0ZSB0aGUgT3BlbkFQSSBKU09OIGZpbGVcbiAqL1xuZXhwb3J0IGNvbnN0IE9wZW5BUEkgPSAoYXBwTW9kdWxlOiBzdHJpbmcsIG91dHB1dFBhdGg6IHN0cmluZyk6IFN0ZXBTcGVjID0+ICh7XG4gIG5hbWU6IFwib3BlbmFwaVwiLFxuICBhY3Rpb246IGB1diBydW4gYXB4IG9wZW5hcGkgJHthcHBNb2R1bGV9ICR7b3V0cHV0UGF0aH1gLFxufSk7XG5cbmV4cG9ydCBpbnRlcmZhY2UgT3J2YWxPdXRwdXRPcHRpb25zIHtcbiAgdGFyZ2V0OiBzdHJpbmc7XG4gIGJhc2VVcmw/OiBzdHJpbmc7XG4gIGNsaWVudD86XG4gICAgfCBcInJlYWN0LXF1ZXJ5XCJcbiAgICB8IFwic3dyXCJcbiAgICB8IFwidnVlLXF1ZXJ5XCJcbiAgICB8IFwic3ZlbHRlLXF1ZXJ5XCJcbiAgICB8IFwiZmV0Y2hcIlxuICAgIHwgXCJheGlvc1wiXG4gICAgfCBcImFuZ3VsYXJcIjtcbiAgaHR0cENsaWVudD86IFwiZmV0Y2hcIiB8IFwiYXhpb3NcIjtcbiAgcHJldHRpZXI/OiBib29sZWFuO1xuICBvdmVycmlkZT86IHtcbiAgICBxdWVyeT86IHtcbiAgICAgIHVzZVF1ZXJ5PzogYm9vbGVhbjtcbiAgICAgIHVzZVN1c3BlbnNlUXVlcnk/OiBib29sZWFuO1xuICAgICAgW2tleTogc3RyaW5nXTogYW55O1xuICAgIH07XG4gICAgW2tleTogc3RyaW5nXTogYW55O1xuICB9O1xuICBba2V5OiBzdHJpbmddOiBhbnk7XG59XG5cbi8qKlxuICogUHJlZGVmaW5lZCBzdGVwIGZvciBnZW5lcmF0aW5nIEFQSSBjbGllbnQgd2l0aCBPcnZhbFxuICogU2tpcHMgZ2VuZXJhdGlvbiBpZiB0aGUgT3BlbkFQSSBzcGVjIGhhc24ndCBjaGFuZ2VkIHNpbmNlIGxhc3QgcnVuXG4gKiBAcGFyYW0gaW5wdXQgLSBQYXRoIHRvIHRoZSBPcGVuQVBJIHNwZWMgZmlsZVxuICogQHBhcmFtIG91dHB1dCAtIE9ydmFsIG91dHB1dCBjb25maWd1cmF0aW9uXG4gKi9cbmV4cG9ydCBjb25zdCBPcnZhbCA9IChpbnB1dDogc3RyaW5nLCBvdXRwdXQ6IE9ydmFsT3V0cHV0T3B0aW9ucyk6IFN0ZXBTcGVjID0+ICh7XG4gIG5hbWU6IFwib3J2YWxcIixcbiAgYWN0aW9uOiBhc3luYyAoKSA9PiB7XG4gICAgLy8gQ2hlY2sgaWYgc3BlYyBmaWxlIGV4aXN0c1xuICAgIGlmICghZXhpc3RzU3luYyhpbnB1dCkpIHtcbiAgICAgIGNvbnNvbGUud2FybihcbiAgICAgICAgYFthcHhdIE9wZW5BUEkgc3BlYyBub3QgZm91bmQgYXQgJHtpbnB1dH0sIHNraXBwaW5nIE9ydmFsIGdlbmVyYXRpb25gLFxuICAgICAgKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICAvLyBSZWFkIGFuZCBoYXNoIHRoZSBzcGVjIGZpbGVcbiAgICBjb25zdCBzcGVjQ29udGVudCA9IHJlYWRGaWxlU3luYyhpbnB1dCwgXCJ1dGYtOFwiKTtcbiAgICBjb25zdCBzcGVjSGFzaCA9IGNyZWF0ZUhhc2goXCJzaGEyNTZcIikudXBkYXRlKHNwZWNDb250ZW50KS5kaWdlc3QoXCJoZXhcIik7XG5cbiAgICAvLyBDaGVjayBpZiBzcGVjIGhhcyBjaGFuZ2VkXG4gICAgY29uc3QgY2FjaGVkSGFzaCA9IHNwZWNIYXNoQ2FjaGUuZ2V0KGlucHV0KTtcbiAgICBpZiAoY2FjaGVkSGFzaCA9PT0gc3BlY0hhc2gpIHtcbiAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBPcGVuQVBJIHNwZWMgdW5jaGFuZ2VkLCBza2lwcGluZyBPcnZhbCBnZW5lcmF0aW9uYCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgLy8gR2VuZXJhdGUgQVBJIGNsaWVudFxuICAgIGF3YWl0IGdlbmVyYXRlKHtcbiAgICAgIGlucHV0LFxuICAgICAgb3V0cHV0LFxuICAgIH0pO1xuXG4gICAgLy8gVXBkYXRlIGNhY2hlXG4gICAgc3BlY0hhc2hDYWNoZS5zZXQoaW5wdXQsIHNwZWNIYXNoKTtcbiAgfSxcbn0pO1xuXG5leHBvcnQgaW50ZXJmYWNlIEFweFBsdWdpbk9wdGlvbnMge1xuICBzdGVwcz86IFN0ZXBTcGVjW107XG4gIGlnbm9yZT86IHN0cmluZ1tdO1xufVxuXG5leHBvcnQgZnVuY3Rpb24gYXB4KG9wdGlvbnM6IEFweFBsdWdpbk9wdGlvbnMgPSB7fSk6IFBsdWdpbiB7XG4gIGNvbnN0IHsgc3RlcHMgPSBbXSwgaWdub3JlID0gW10gfSA9IG9wdGlvbnM7XG5cbiAgbGV0IG91dERpcjogc3RyaW5nO1xuICBsZXQgdGltZXI6IE5vZGVKUy5UaW1lb3V0IHwgbnVsbCA9IG51bGw7XG4gIGxldCBzdG9wcGluZyA9IGZhbHNlO1xuICBsZXQgcmVzb2x2ZWRJZ25vcmVzOiBzdHJpbmdbXSA9IFtdO1xuICBsZXQgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZykgcmV0dXJuO1xuXG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgaWYgKHR5cGVvZiBhY3Rpb24gPT09IFwic3RyaW5nXCIpIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgc2hlbGwgY29tbWFuZFxuICAgICAgY29uc3QgeyBzdGRvdXQsIHN0ZGVyciB9ID0gYXdhaXQgZXhlY0FzeW5jKGFjdGlvbik7XG4gICAgICBpZiAoc3Rkb3V0KSBjb25zb2xlLmxvZyhzdGRvdXQudHJpbSgpKTtcbiAgICAgIGlmIChzdGRlcnIpIGNvbnNvbGUuZXJyb3Ioc3RkZXJyLnRyaW0oKSk7XG4gICAgfSBlbHNlIHtcbiAgICAgIC8vIEV4ZWN1dGUgYXMgZnVuY3Rpb25cbiAgICAgIGF3YWl0IGFjdGlvbigpO1xuICAgIH1cbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgfVxuXG4gIGFzeW5jIGZ1bmN0aW9uIHJ1bkFsbFN0ZXBzKCk6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmIChzdG9wcGluZyB8fCBpc1J1bm5pbmdTdGVwcykgcmV0dXJuO1xuXG4gICAgaXNSdW5uaW5nU3RlcHMgPSB0cnVlO1xuXG4gICAgdHJ5IHtcbiAgICAgIGZvciAoY29uc3Qgc3RlcCBvZiBzdGVwcykge1xuICAgICAgICBpZiAoc3RvcHBpbmcpIGJyZWFrO1xuICAgICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICAgIHRyeSB7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgICBhd2FpdCBleGVjdXRlQWN0aW9uKHN0ZXAuYWN0aW9uKTtcbiAgICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKckyAoJHtEYXRlLm5vdygpIC0gc3RhcnR9IG1zKWApO1xuICAgICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyXYCwgZXJyKTtcbiAgICAgICAgICB0aHJvdyBlcnI7XG4gICAgICAgIH1cbiAgICAgIH1cbiAgICB9IGZpbmFsbHkge1xuICAgICAgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcbiAgICB9XG4gIH1cblxuICAvKipcbiAgICogRW5zdXJlcyB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHMgYW5kIGNvbnRhaW5zIGEgLmdpdGlnbm9yZSBmaWxlLlxuICAgKiBUaGlzIGlzIGNhbGxlZCBhdCBtdWx0aXBsZSBwb2ludHMgdG8gZ3VhcmFudGVlIHRoZSBkaXJlY3RvcnkgaXMgYWx3YXlzIHByZXNlbnQuXG4gICAqL1xuICBmdW5jdGlvbiBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIG91dERpciBpcyBub3Qgc2V0YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgdHJ5IHtcbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzXG4gICAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICAgIH1cblxuICAgICAgLy8gQWx3YXlzIGVuc3VyZSAuZ2l0aWdub3JlIGV4aXN0cyBpbiBvdXRwdXQgZGlyZWN0b3J5XG4gICAgICBjb25zdCBnaXRpZ25vcmVQYXRoID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgICB3cml0ZUZpbGVTeW5jKGdpdGlnbm9yZVBhdGgsIFwiKlxcblwiKTtcbiAgICAgIH1cbiAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIGZhaWxlZCB0byBlbnN1cmUgb3V0cHV0IGRpcmVjdG9yeTpgLCBlcnIpO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikge1xuICAgICAgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gbnVsbDtcbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiByZXNldCgpOiB2b2lkIHtcbiAgICBzdG9wcGluZyA9IGZhbHNlO1xuICAgIHRpbWVyID0gbnVsbDtcbiAgICBpc1J1bm5pbmdTdGVwcyA9IGZhbHNlO1xuICB9XG5cbiAgcmV0dXJuIHtcbiAgICBuYW1lOiBcImFweFwiLFxuICAgIGFwcGx5OiAoKSA9PiB0cnVlLFxuXG4gICAgY29uZmlnUmVzb2x2ZWQoY29uZmlnKSB7XG4gICAgICBvdXREaXIgPSByZXNvbHZlKGNvbmZpZy5yb290LCBjb25maWcuYnVpbGQub3V0RGlyKTtcbiAgICAgIHJlc29sdmVkSWdub3JlcyA9IGlnbm9yZS5tYXAoKHBhdHRlcm4pID0+XG4gICAgICAgIHJlc29sdmUocHJvY2Vzcy5jd2QoKSwgcGF0dGVybiksXG4gICAgICApO1xuXG4gICAgICAvLyBSZXNldCBzdGF0ZSBmb3IgbmV3IGJ1aWxkXG4gICAgICByZXNldCgpO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhcyBzb29uIGFzIHdlIGtub3cgdGhlIG91dERpclxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNvbmZpZ3VyZVNlcnZlcihzZXJ2ZXIpIHtcbiAgICAgIHNlcnZlci5odHRwU2VydmVyPy5vbmNlKFwiY2xvc2VcIiwgc3RvcCk7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIHdoZW4gc2VydmVyIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGFzeW5jIGJ1aWxkU3RhcnQoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgYnVpbGQgc3RhcnRzXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb24gZXZlcnkgSE1SIHVwZGF0ZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIC8vIENoZWNrIGlmIGZpbGUgc2hvdWxkIGJlIGlnbm9yZWRcbiAgICAgIGlmIChyZXNvbHZlZElnbm9yZXMuc29tZSgocGF0dGVybikgPT4gY3R4LmZpbGUuaW5jbHVkZXMocGF0dGVybikpKSB7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgLy8gRGVib3VuY2Ugc3RlcCBleGVjdXRpb24gb24gSE1SIHVwZGF0ZXNcbiAgICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcblxuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KGFzeW5jICgpID0+IHtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGJlZm9yZSBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgICBhd2FpdCBydW5BbGxTdGVwcygpO1xuXG4gICAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICB9LCAxMDApO1xuXG4gICAgICAvLyBBbGxvdyB0aGUgcHJvY2VzcyB0byBleGl0IGV2ZW4gaWYgdGhpcyB0aW1lciBpcyBwZW5kaW5nXG4gICAgICB0aW1lci51bnJlZigpO1xuICAgIH0sXG5cbiAgICB3cml0ZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIGFmdGVyIGZpbGVzIGFyZSB3cml0dGVuXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICB9LFxuXG4gICAgY2xvc2VCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBvbmUgZmluYWwgdGltZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICBzdG9wKCk7XG4gICAgfSxcbiAgfTtcbn1cblxuLy8gRGVmYXVsdCBleHBvcnQgZm9yIGNvbnZlbmllbmNlOiBpbXBvcnQgYXB4IGZyb20gXCJhcHhcIlxuZXhwb3J0IGRlZmF1bHQgYXB4O1xuIgogIF0sCiAgIm1hcHBpbmdzIjogIjtBQUFBO0FBQ0E7QUFFQTtBQUNBO0FBQ0E7QUFDQTtBQThGTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7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eEIsR0FBRztBQUdOLFlBQU0sTUFBTTtBQUFBO0FBQUEsSUFHZCxXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFBQTtBQUFBLElBRzNCLFdBQVcsR0FBRztBQUVaLCtCQUF5QjtBQUN6QixXQUFLO0FBQUE7QUFBQSxFQUVUO0FBQUE7QUEzUEYsSUFBTSxZQUFZLFVBQVUsSUFBSTtBQUdoQyxJQUFNLGdCQUFnQixJQUFJO0FBU25CLElBQU0sT0FBTyxDQUFDLFNBQTZCO0FBTzNDLElBQU0sVUFBVSxDQUFDLFdBQW1CLGdCQUFrQztBQUFBLEVBQzNFLE1BQU07QUFBQSxFQUNOLFFBQVEsc0JBQXNCLGFBQWE7QUFDN0M7QUFnQ08sSUFBTSxRQUFRLENBQUMsT0FBZSxZQUEwQztBQUFBLEVBQzdFLE1BQU07QUFBQSxFQUNOLFFBQVEsWUFBWTtBQUVsQixTQUFLLFdBQVcsS0FBSyxHQUFHO0FBQ3RCLGNBQVEsS0FDTixtQ0FBbUMsa0NBQ3JDO0FBQ0E7QUFBQSxJQUNGO0FBR0EsVUFBTSxjQUFjLGFBQWEsT0FBTyxPQUFPO0FBQy9DLFVBQU0sV0FBVyxXQUFXLFFBQVEsRUFBRSxPQUFPLFdBQVcsRUFBRSxPQUFPLEtBQUs7QUFHdEUsVUFBTSxhQUFhLGNBQWMsSUFBSSxLQUFLO0FBQzFDLFFBQUksZUFBZSxVQUFVO0FBQzNCLGNBQVEsSUFBSSx5REFBeUQ7QUFDckU7QUFBQSxJQUNGO0FBR0EsVUFBTSxTQUFTO0FBQUEsTUFDYjtBQUFBLE1BQ0E7QUFBQSxJQUNGLENBQUM7QUFHRCxrQkFBYyxJQUFJLE9BQU8sUUFBUTtBQUFBO0FBRXJDO0FBMEtBLElBQWU7IiwKICAiZGVidWdJZCI6ICJBRkZBRkRBRUVCNTFFQjEzNjQ3NTZFMjE2NDc1NkUyMSIsCiAgIm5hbWVzIjogW10KfQ==
