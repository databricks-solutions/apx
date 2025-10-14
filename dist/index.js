// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { exec } from "child_process";
import { promisify } from "util";
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
var Step = (spec) => spec;
var OpenAPI = (appModule, outputPath) => ({
  name: "openapi",
  action: `uv run apx openapi ${appModule} ${outputPath}`
});
var Orval = (config) => ({
  name: "orval",
  action: () => generate(config)
});
var plugins_default = apx;
export {
  plugins_default as default,
  apx,
  Step,
  Orval,
  OpenAPI
};

//# debugId=A2DC283FE6852E4764756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPcHRpb25zRXhwb3J0IGFzIE9ydmFsQ29uZmlnIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gUmUtZXhwb3J0IE9ydmFsQ29uZmlnIGZvciBjb252ZW5pZW5jZVxuZXhwb3J0IHR5cGUgeyBPcnZhbENvbmZpZyB9O1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBAcGFyYW0gY29uZmlnIC0gT3J2YWwgY29uZmlndXJhdGlvbiBvYmplY3RcbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKGNvbmZpZzogT3J2YWxDb25maWcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogKCkgPT4gZ2VuZXJhdGUoY29uZmlnKSxcbn0pO1xuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgbGV0IGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG5cbiAgYXN5bmMgZnVuY3Rpb24gZXhlY3V0ZUFjdGlvbihhY3Rpb246IFN0ZXBBY3Rpb24pOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBcbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICBpZiAodHlwZW9mIGFjdGlvbiA9PT0gXCJzdHJpbmdcIikge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBzaGVsbCBjb21tYW5kXG4gICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uKTtcbiAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgaWYgKHN0ZGVycikgY29uc29sZS5lcnJvcihzdGRlcnIudHJpbSgpKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgYXdhaXQgYWN0aW9uKCk7XG4gICAgfVxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gcnVuQWxsU3RlcHMoKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgaWYgKHN0b3BwaW5nIHx8IGlzUnVubmluZ1N0ZXBzKSByZXR1cm47XG4gICAgXG4gICAgaXNSdW5uaW5nU3RlcHMgPSB0cnVlO1xuICAgIFxuICAgIHRyeSB7XG4gICAgICBmb3IgKGNvbnN0IHN0ZXAgb2Ygc3RlcHMpIHtcbiAgICAgICAgaWYgKHN0b3BwaW5nKSBicmVhaztcbiAgICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgICB0cnkge1xuICAgICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4o+zYCk7XG4gICAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDinJMgKCR7RGF0ZS5ub3coKSAtIHN0YXJ0fSBtcylgKTtcbiAgICAgICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgICAgdGhyb3cgZXJyO1xuICAgICAgICB9XG4gICAgICB9XG4gICAgfSBmaW5hbGx5IHtcbiAgICAgIGlzUnVubmluZ1N0ZXBzID0gZmFsc2U7XG4gICAgfVxuICB9XG5cbiAgLyoqXG4gICAqIEVuc3VyZXMgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzIGFuZCBjb250YWlucyBhIC5naXRpZ25vcmUgZmlsZS5cbiAgICogVGhpcyBpcyBjYWxsZWQgYXQgbXVsdGlwbGUgcG9pbnRzIHRvIGd1YXJhbnRlZSB0aGUgZGlyZWN0b3J5IGlzIGFsd2F5cyBwcmVzZW50LlxuICAgKi9cbiAgZnVuY3Rpb24gZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk6IHZvaWQge1xuICAgIGlmICghb3V0RGlyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBvdXREaXIgaXMgbm90IHNldGApO1xuICAgICAgcmV0dXJuO1xuICAgIH1cblxuICAgIHRyeSB7XG4gICAgICAvLyBBbHdheXMgZW5zdXJlIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGV4aXN0c1xuICAgICAgaWYgKCFleGlzdHNTeW5jKG91dERpcikpIHtcbiAgICAgICAgbWtkaXJTeW5jKG91dERpciwgeyByZWN1cnNpdmU6IHRydWUgfSk7XG4gICAgICB9XG5cbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgICAgY29uc3QgZ2l0aWdub3JlUGF0aCA9IGpvaW4ob3V0RGlyLCBcIi5naXRpZ25vcmVcIik7XG4gICAgICBpZiAoIWV4aXN0c1N5bmMoZ2l0aWdub3JlUGF0aCkpIHtcbiAgICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICB9XG4gICAgfSBjYXRjaCAoZXJyKSB7XG4gICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSBmYWlsZWQgdG8gZW5zdXJlIG91dHB1dCBkaXJlY3Rvcnk6YCwgZXJyKTtcbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiBzdG9wKCk6IHZvaWQge1xuICAgIGlmIChzdG9wcGluZykgcmV0dXJuO1xuICAgIHN0b3BwaW5nID0gdHJ1ZTtcbiAgICBpZiAodGltZXIpIHtcbiAgICAgIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB0aW1lciA9IG51bGw7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gICAgaXNSdW5uaW5nU3RlcHMgPSBmYWxzZTtcbiAgfVxuXG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJhcHhcIixcbiAgICBhcHBseTogKCkgPT4gdHJ1ZSxcblxuICAgIGNvbmZpZ1Jlc29sdmVkKGNvbmZpZykge1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyB3aGVuIHNlcnZlciBzdGFydHNcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIGJ1aWxkIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uIGV2ZXJ5IEhNUiB1cGRhdGVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuXG4gICAgICAvLyBDaGVjayBpZiBmaWxlIHNob3VsZCBiZSBpZ25vcmVkXG4gICAgICBpZiAocmVzb2x2ZWRJZ25vcmVzLnNvbWUoKHBhdHRlcm4pID0+IGN0eC5maWxlLmluY2x1ZGVzKHBhdHRlcm4pKSkge1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIC8vIERlYm91bmNlIHN0ZXAgZXhlY3V0aW9uIG9uIEhNUiB1cGRhdGVzXG4gICAgICBpZiAodGltZXIpIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICBcbiAgICAgIHRpbWVyID0gc2V0VGltZW91dChhc3luYyAoKSA9PiB7XG4gICAgICAgIHRpbWVyID0gbnVsbDtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgfSwgMTAwKTtcbiAgICAgIFxuICAgICAgLy8gQWxsb3cgdGhlIHByb2Nlc3MgdG8gZXhpdCBldmVuIGlmIHRoaXMgdGltZXIgaXMgcGVuZGluZ1xuICAgICAgdGltZXIudW5yZWYoKTtcbiAgICB9LFxuXG4gICAgd3JpdGVCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBmaWxlcyBhcmUgd3JpdHRlblxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb25lIGZpbmFsIHRpbWVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDtcbiIKICBdLAogICJtYXBwaW5ncyI6ICI7QUFBQTtBQUNBO0FBRUE7QUFDQTtBQUNBO0FBdUNPLFNBQVMsR0FBRyxDQUFDLFVBQTRCLENBQUMsR0FBVztBQUMxRCxVQUFRLFFBQVEsQ0FBQyxHQUFHLFNBQVMsQ0FBQyxNQUFNO0FBRXBDLE1BQUk7QUFDSixNQUFJLFFBQStCO0FBQ25DLE1BQUksV0FBVztBQUNmLE1BQUksa0JBQTRCLENBQUM7QUFDakMsTUFBSSxpQkFBaUI7QUFFckIsaUJBQWUsYUFBYSxDQUFDLFFBQW1DO0FBQzlELFFBQUk7QUFBVTtBQUVkLDZCQUF5QjtBQUN6QixlQUFXLFdBQVcsVUFBVTtBQUU5QixjQUFRLFFBQVEsV0FBVyxNQUFNLFVBQVUsTUFBTTtBQUNqRCxVQUFJO0FBQVEsZ0JBQVEsSUFBSSxPQUFPLEtBQUssQ0FBQztBQUNyQyxVQUFJO0FBQVEsZ0JBQVEsTUFBTSxPQUFPLEtBQUssQ0FBQztBQUFBLElBQ3pDLE9BQU87QUFFTCxZQUFNLE9BQU87QUFBQTtBQUVmLDZCQUF5QjtBQUFBO0FBRzNCLGlCQUFlLFdBQVcsR0FBa0I7QUFDMUMsUUFBSSxZQUFZO0FBQWdCO0FBRWhDLHFCQUFpQjtBQUVqQixRQUFJO0FBQ0YsaUJBQVcsUUFBUSxPQUFPO0FBQ3hCLFlBQUk7QUFBVTtBQUNkLGNBQU0sUUFBUSxLQUFLLElBQUk7QUFDdkIsWUFBSTtBQUNGLGtCQUFRLElBQUksU0FBUyxLQUFLLGFBQU87QUFDakMsZ0JBQU0sY0FBYyxLQUFLLE1BQU07QUFDL0Isa0JBQVEsSUFBSSxTQUFTLEtBQUssZ0JBQVUsS0FBSyxJQUFJLElBQUksV0FBVztBQUFBLGlCQUNyRCxLQUFQO0FBQ0Esa0JBQVEsTUFBTSxTQUFTLEtBQUssZUFBUyxHQUFHO0FBQ3hDLGdCQUFNO0FBQUE7QUFBQSxNQUVWO0FBQUEsY0FDQTtBQUNBLHVCQUFpQjtBQUFBO0FBQUE7QUFRckIsV0FBUyx3QkFBd0IsR0FBUztBQUN4QyxTQUFLLFFBQVE7QUFDWCxjQUFRLE1BQU0seUJBQXlCO0FBQ3ZDO0FBQUEsSUFDRjtBQUVBLFFBQUk7QUFFRixXQUFLLFdBQVcsTUFBTSxHQUFHO0FBQ3ZCLGtCQUFVLFFBQVEsRUFBRSxXQUFXLEtBQUssQ0FBQztBQUFBLE1BQ3ZDO0FBR0EsWUFBTSxnQkFBZ0IsS0FBSyxRQUFRLFlBQVk7QUFDL0MsV0FBSyxXQUFXLGFBQWEsR0FBRztBQUM5QixzQkFBYyxlQUFlLEtBQUs7QUFBQSxNQUNwQztBQUFBLGFBQ08sS0FBUDtBQUNBLGNBQVEsTUFBTSw0Q0FBNEMsR0FBRztBQUFBO0FBQUE7QUFJakUsV0FBUyxJQUFJLEdBQVM7QUFDcEIsUUFBSTtBQUFVO0FBQ2QsZUFBVztBQUNYLFFBQUksT0FBTztBQUNULG1CQUFhLEtBQUs7QUFDbEIsY0FBUTtBQUFBLElBQ1Y7QUFBQTtBQUdGLFdBQVMsS0FBSyxHQUFTO0FBQ3JCLGVBQVc7QUFDWCxZQUFRO0FBQ1IscUJBQWlCO0FBQUE7QUFHbkIsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFFYixjQUFjLENBQUMsUUFBUTtBQUNyQixlQUFTLFFBQVEsT0FBTyxNQUFNLE9BQU8sTUFBTSxNQUFNO0FBQ2pELHdCQUFrQixPQUFPLElBQUksQ0FBQyxZQUM1QixRQUFRLFFBQVEsSUFBSSxHQUFHLE9BQU8sQ0FDaEM7QUFHQSxZQUFNO0FBR04sK0JBQXlCO0FBQUE7QUFBQSxJQUczQixlQUFlLENBQUMsUUFBUTtBQUN0QixhQUFPLFlBQVksS0FBSyxTQUFTLElBQUk7QUFHckMsK0JBQXlCO0FBQUE7QUFBQSxTQUdyQixXQUFVLEdBQUc7QUFFakIsK0JBQXlCO0FBRXpCLFVBQUksTUFBTSxTQUFTLEdBQUc7QUFDcEIsY0FBTSxZQUFZO0FBQUEsTUFDcEI7QUFBQTtBQUFBLElBR0YsZUFBZSxDQUFDLEtBQUs7QUFFbkIsK0JBQXlCO0FBR3pCLFVBQUksZ0JBQWdCLEtBQUssQ0FBQyxZQUFZLElBQUksS0FBSyxTQUFTLE9BQU8sQ0FBQyxHQUFHO0FBQ2pFO0FBQUEsTUFDRjtBQUdBLFVBQUk7QUFBTyxxQkFBYSxLQUFLO0FBRTdCLGNBQVEsV0FBVyxZQUFZO0FBQzdCLGdCQUFRO0FBR1IsaUNBQXlCO0FBQ3pCLGNBQU0sWUFBWTtBQUdsQixpQ0FBeUI7QUFBQSxTQUN4QixHQUFHO0FBR04sWUFBTSxNQUFNO0FBQUE7QUFBQSxJQUdkLFdBQVcsR0FBRztBQUVaLCtCQUF5QjtBQUFBO0FBQUEsSUFHM0IsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQ3pCLFdBQUs7QUFBQTtBQUFBLEVBRVQ7QUFBQTtBQXBNRixJQUFNLFlBQVksVUFBVSxJQUFJO0FBWXpCLElBQU0sT0FBTyxDQUFDLFNBQTZCO0FBTzNDLElBQU0sVUFBVSxDQUFDLFdBQW1CLGdCQUFrQztBQUFBLEVBQzNFLE1BQU07QUFBQSxFQUNOLFFBQVEsc0JBQXNCLGFBQWE7QUFDN0M7QUFNTyxJQUFNLFFBQVEsQ0FBQyxZQUFtQztBQUFBLEVBQ3ZELE1BQU07QUFBQSxFQUNOLFFBQVEsTUFBTSxTQUFTLE1BQU07QUFDL0I7QUF5S0EsSUFBZTsiLAogICJkZWJ1Z0lkIjogIkEyREMyODNGRTY4NTJFNDc2NDc1NkUyMTY0NzU2RTIxIiwKICAibmFtZXMiOiBbXQp9
