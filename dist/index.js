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
  const signalHandlers = new Map;
  async function executeAction(action) {
    console.log(`[apx] executing action: ${action}`);
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
    for (const [signal, handler] of signalHandlers) {
      process.off(signal, handler);
    }
    signalHandlers.clear();
  }
  function reset() {
    stopping = false;
    timer = null;
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      if (signalHandlers.size === 0) {
        const handleSIGINT = () => stop();
        const handleSIGTERM = () => stop();
        signalHandlers.set("SIGINT", handleSIGINT);
        signalHandlers.set("SIGTERM", handleSIGTERM);
        process.on("SIGINT", handleSIGINT);
        process.on("SIGTERM", handleSIGTERM);
      }
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

//# debugId=6F996B6DDE22EC6E64756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPcHRpb25zRXhwb3J0IGFzIE9ydmFsQ29uZmlnIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gUmUtZXhwb3J0IE9ydmFsQ29uZmlnIGZvciBjb252ZW5pZW5jZVxuZXhwb3J0IHR5cGUgeyBPcnZhbENvbmZpZyB9O1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBAcGFyYW0gY29uZmlnIC0gT3J2YWwgY29uZmlndXJhdGlvbiBvYmplY3RcbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKGNvbmZpZzogT3J2YWxDb25maWcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogKCkgPT4gZ2VuZXJhdGUoY29uZmlnKSxcbn0pO1xuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgXG4gIC8vIFN0b3JlIHNpZ25hbCBoYW5kbGVyIHJlZmVyZW5jZXMgZm9yIGNsZWFudXBcbiAgY29uc3Qgc2lnbmFsSGFuZGxlcnMgPSBuZXcgTWFwPE5vZGVKUy5TaWduYWxzLCAoKSA9PiB2b2lkPigpO1xuXG4gIGFzeW5jIGZ1bmN0aW9uIGV4ZWN1dGVBY3Rpb24oYWN0aW9uOiBTdGVwQWN0aW9uKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgY29uc29sZS5sb2coYFthcHhdIGV4ZWN1dGluZyBhY3Rpb246ICR7YWN0aW9ufWApO1xuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIGlmICh0eXBlb2YgYWN0aW9uID09PSBcInN0cmluZ1wiKSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIHNoZWxsIGNvbW1hbmRcbiAgICAgIGNvbnN0IHsgc3Rkb3V0LCBzdGRlcnIgfSA9IGF3YWl0IGV4ZWNBc3luYyhhY3Rpb24pO1xuICAgICAgaWYgKHN0ZG91dCkgY29uc29sZS5sb2coc3Rkb3V0LnRyaW0oKSk7XG4gICAgICBpZiAoc3RkZXJyKSBjb25zb2xlLmVycm9yKHN0ZGVyci50cmltKCkpO1xuICAgIH0gZWxzZSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIGZ1bmN0aW9uXG4gICAgICBhd2FpdCBhY3Rpb24oKTtcbiAgICB9XG4gICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gIH1cblxuICBhc3luYyBmdW5jdGlvbiBydW5BbGxTdGVwcygpOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBmb3IgKGNvbnN0IHN0ZXAgb2Ygc3RlcHMpIHtcbiAgICAgIGlmIChzdG9wcGluZykgYnJlYWs7XG4gICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICB0cnkge1xuICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKPs2ApO1xuICAgICAgICBhd2FpdCBleGVjdXRlQWN0aW9uKHN0ZXAuYWN0aW9uKTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDinJMgKCR7RGF0ZS5ub3coKSAtIHN0YXJ0fSBtcylgKTtcbiAgICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyXYCwgZXJyKTtcbiAgICAgICAgdGhyb3cgZXJyO1xuICAgICAgfVxuICAgIH1cbiAgfVxuXG4gIC8qKlxuICAgKiBFbnN1cmVzIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGV4aXN0cyBhbmQgY29udGFpbnMgYSAuZ2l0aWdub3JlIGZpbGUuXG4gICAqIFRoaXMgaXMgY2FsbGVkIGF0IG11bHRpcGxlIHBvaW50cyB0byBndWFyYW50ZWUgdGhlIGRpcmVjdG9yeSBpcyBhbHdheXMgcHJlc2VudC5cbiAgICovXG4gIGZ1bmN0aW9uIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpOiB2b2lkIHtcbiAgICBpZiAoIW91dERpcikge1xuICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gb3V0RGlyIGlzIG5vdCBzZXRgKTtcbiAgICAgIHJldHVybjtcbiAgICB9XG5cbiAgICB0cnkge1xuICAgICAgLy8gQWx3YXlzIGVuc3VyZSB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHNcbiAgICAgIGlmICghZXhpc3RzU3luYyhvdXREaXIpKSB7XG4gICAgICAgIG1rZGlyU3luYyhvdXREaXIsIHsgcmVjdXJzaXZlOiB0cnVlIH0pO1xuICAgICAgfVxuXG4gICAgICAvLyBBbHdheXMgZW5zdXJlIC5naXRpZ25vcmUgZXhpc3RzIGluIG91dHB1dCBkaXJlY3RvcnlcbiAgICAgIGNvbnN0IGdpdGlnbm9yZVBhdGggPSBqb2luKG91dERpciwgXCIuZ2l0aWdub3JlXCIpO1xuICAgICAgaWYgKCFleGlzdHNTeW5jKGdpdGlnbm9yZVBhdGgpKSB7XG4gICAgICAgIHdyaXRlRmlsZVN5bmMoZ2l0aWdub3JlUGF0aCwgXCIqXFxuXCIpO1xuICAgICAgfVxuICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gZmFpbGVkIHRvIGVuc3VyZSBvdXRwdXQgZGlyZWN0b3J5OmAsIGVycik7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gc3RvcCgpOiB2b2lkIHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBzdG9wcGluZyA9IHRydWU7XG4gICAgaWYgKHRpbWVyKSB7XG4gICAgICBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgICAgdGltZXIgPSBudWxsO1xuICAgIH1cbiAgICBcbiAgICAvLyBDbGVhbiB1cCBzaWduYWwgaGFuZGxlcnMgdG8gcHJldmVudCBpbnRlcmZlcmVuY2Ugd2l0aCBWaXRlJ3Mgc2h1dGRvd25cbiAgICBmb3IgKGNvbnN0IFtzaWduYWwsIGhhbmRsZXJdIG9mIHNpZ25hbEhhbmRsZXJzKSB7XG4gICAgICBwcm9jZXNzLm9mZihzaWduYWwsIGhhbmRsZXIpO1xuICAgIH1cbiAgICBzaWduYWxIYW5kbGVycy5jbGVhcigpO1xuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG5cbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IHJlc29sdmUoY29uZmlnLnJvb3QsIGNvbmZpZy5idWlsZC5vdXREaXIpO1xuICAgICAgcmVzb2x2ZWRJZ25vcmVzID0gaWdub3JlLm1hcCgocGF0dGVybikgPT5cbiAgICAgICAgcmVzb2x2ZShwcm9jZXNzLmN3ZCgpLCBwYXR0ZXJuKSxcbiAgICAgICk7XG5cbiAgICAgIC8vIFJlc2V0IHN0YXRlIGZvciBuZXcgYnVpbGRcbiAgICAgIHJlc2V0KCk7XG5cbiAgICAgIC8vIFNldHVwIHNpZ25hbCBoYW5kbGVycyBmb3IgZ3JhY2VmdWwgc2h1dGRvd24gKG9ubHkgaWYgbm90IGFscmVhZHkgcmVnaXN0ZXJlZClcbiAgICAgIGlmIChzaWduYWxIYW5kbGVycy5zaXplID09PSAwKSB7XG4gICAgICAgIGNvbnN0IGhhbmRsZVNJR0lOVCA9ICgpID0+IHN0b3AoKTtcbiAgICAgICAgY29uc3QgaGFuZGxlU0lHVEVSTSA9ICgpID0+IHN0b3AoKTtcbiAgICAgICAgXG4gICAgICAgIHNpZ25hbEhhbmRsZXJzLnNldChcIlNJR0lOVFwiLCBoYW5kbGVTSUdJTlQpO1xuICAgICAgICBzaWduYWxIYW5kbGVycy5zZXQoXCJTSUdURVJNXCIsIGhhbmRsZVNJR1RFUk0pO1xuICAgICAgICBcbiAgICAgICAgcHJvY2Vzcy5vbihcIlNJR0lOVFwiLCBoYW5kbGVTSUdJTlQpO1xuICAgICAgICBwcm9jZXNzLm9uKFwiU0lHVEVSTVwiLCBoYW5kbGVTSUdURVJNKTtcbiAgICAgIH1cblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyB3aGVuIHNlcnZlciBzdGFydHNcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIGJ1aWxkIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uIGV2ZXJ5IEhNUiB1cGRhdGVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuXG4gICAgICAvLyBDaGVjayBpZiBmaWxlIHNob3VsZCBiZSBpZ25vcmVkXG4gICAgICBpZiAocmVzb2x2ZWRJZ25vcmVzLnNvbWUoKHBhdHRlcm4pID0+IGN0eC5maWxlLmluY2x1ZGVzKHBhdHRlcm4pKSkge1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIC8vIERlYm91bmNlIHN0ZXAgZXhlY3V0aW9uIG9uIEhNUiB1cGRhdGVzXG4gICAgICBpZiAodGltZXIpIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB0aW1lciA9IHNldFRpbWVvdXQoYXN5bmMgKCkgPT4ge1xuICAgICAgICB0aW1lciA9IG51bGw7XG5cbiAgICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG5cbiAgICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYWZ0ZXIgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgIH0sIDEwMCk7XG4gICAgfSxcblxuICAgIHdyaXRlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYWZ0ZXIgZmlsZXMgYXJlIHdyaXR0ZW5cbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjbG9zZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uZSBmaW5hbCB0aW1lXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgIHN0b3AoKTtcbiAgICB9LFxuICB9O1xufVxuXG4vLyBEZWZhdWx0IGV4cG9ydCBmb3IgY29udmVuaWVuY2U6IGltcG9ydCBhcHggZnJvbSBcImFweFwiXG5leHBvcnQgZGVmYXVsdCBhcHg7XG4iCiAgXSwKICAibWFwcGluZ3MiOiAiO0FBQUE7QUFDQTtBQUVBO0FBQ0E7QUFDQTtBQXVDTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFT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SSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBR0EsVUFBSTtBQUFPLHFCQUFhLEtBQUs7QUFDN0IsY0FBUSxXQUFXLFlBQVk7QUFDN0IsZ0JBQVE7QUFHUixpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFBQTtBQUFBLElBR1IsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBMU1GLElBQU0sWUFBWSxVQUFVLElBQUk7QUFZekIsSUFBTSxPQUFPLENBQUMsU0FBNkI7QUFPM0MsSUFBTSxVQUFVLENBQUMsV0FBbUIsZ0JBQWtDO0FBQUEsRUFDM0UsTUFBTTtBQUFBLEVBQ04sUUFBUSxzQkFBc0IsYUFBYTtBQUM3QztBQU1PLElBQU0sUUFBUSxDQUFDLFlBQW1DO0FBQUEsRUFDdkQsTUFBTTtBQUFBLEVBQ04sUUFBUSxNQUFNLFNBQVMsTUFBTTtBQUMvQjtBQStLQSxJQUFlOyIsCiAgImRlYnVnSWQiOiAiNkY5OTZCNkRERTIyRUM2RTY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
