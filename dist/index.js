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

//# debugId=FDD01CB88AC1A49264756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5pbXBvcnQgeyBnZW5lcmF0ZSwgdHlwZSBPcHRpb25zRXhwb3J0IGFzIE9ydmFsQ29uZmlnIH0gZnJvbSBcIm9ydmFsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuLy8gUmUtZXhwb3J0IE9ydmFsQ29uZmlnIGZvciBjb252ZW5pZW5jZVxuZXhwb3J0IHR5cGUgeyBPcnZhbENvbmZpZyB9O1xuXG5leHBvcnQgdHlwZSBTdGVwQWN0aW9uID0gc3RyaW5nIHwgKCgpID0+IHZvaWQgfCBQcm9taXNlPHZvaWQ+KTtcblxuZXhwb3J0IHR5cGUgU3RlcFNwZWMgPSB7XG4gIG5hbWU6IHN0cmluZztcbiAgYWN0aW9uOiBTdGVwQWN0aW9uO1xufTtcblxuZXhwb3J0IGNvbnN0IFN0ZXAgPSAoc3BlYzogU3RlcFNwZWMpOiBTdGVwU3BlYyA9PiBzcGVjO1xuXG4vKipcbiAqIFByZWRlZmluZWQgc3RlcCBmb3IgZ2VuZXJhdGluZyBPcGVuQVBJIHNjaGVtYVxuICogQHBhcmFtIGFwcE1vZHVsZSAtIFRoZSBQeXRob24gbW9kdWxlIHBhdGggKGUuZy4sIFwic2FtcGxlLmFwaS5hcHA6YXBwXCIpXG4gKiBAcGFyYW0gb3V0cHV0UGF0aCAtIFdoZXJlIHRvIHdyaXRlIHRoZSBPcGVuQVBJIEpTT04gZmlsZVxuICovXG5leHBvcnQgY29uc3QgT3BlbkFQSSA9IChhcHBNb2R1bGU6IHN0cmluZywgb3V0cHV0UGF0aDogc3RyaW5nKTogU3RlcFNwZWMgPT4gKHtcbiAgbmFtZTogXCJvcGVuYXBpXCIsXG4gIGFjdGlvbjogYHV2IHJ1biBhcHggb3BlbmFwaSAke2FwcE1vZHVsZX0gJHtvdXRwdXRQYXRofWAsXG59KTtcblxuLyoqXG4gKiBQcmVkZWZpbmVkIHN0ZXAgZm9yIGdlbmVyYXRpbmcgQVBJIGNsaWVudCB3aXRoIE9ydmFsXG4gKiBAcGFyYW0gY29uZmlnIC0gT3J2YWwgY29uZmlndXJhdGlvbiBvYmplY3RcbiAqL1xuZXhwb3J0IGNvbnN0IE9ydmFsID0gKGNvbmZpZzogT3J2YWxDb25maWcpOiBTdGVwU3BlYyA9PiAoe1xuICBuYW1lOiBcIm9ydmFsXCIsXG4gIGFjdGlvbjogKCkgPT4gZ2VuZXJhdGUoY29uZmlnKSxcbn0pO1xuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBleGVjdXRpbmcgYWN0aW9uOiAke2FjdGlvbn1gKTtcbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICBpZiAodHlwZW9mIGFjdGlvbiA9PT0gXCJzdHJpbmdcIikge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBzaGVsbCBjb21tYW5kXG4gICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uKTtcbiAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgaWYgKHN0ZGVycikgY29uc29sZS5lcnJvcihzdGRlcnIudHJpbSgpKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgYXdhaXQgYWN0aW9uKCk7XG4gICAgfVxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gcnVuQWxsU3RlcHMoKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgZm9yIChjb25zdCBzdGVwIG9mIHN0ZXBzKSB7XG4gICAgICBpZiAoc3RvcHBpbmcpIGJyZWFrO1xuICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgdHJ5IHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyTICgke0RhdGUubm93KCkgLSBzdGFydH0gbXMpYCk7XG4gICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgIHRocm93IGVycjtcbiAgICAgIH1cbiAgICB9XG4gIH1cblxuICAvKipcbiAgICogRW5zdXJlcyB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHMgYW5kIGNvbnRhaW5zIGEgLmdpdGlnbm9yZSBmaWxlLlxuICAgKiBUaGlzIGlzIGNhbGxlZCBhdCBtdWx0aXBsZSBwb2ludHMgdG8gZ3VhcmFudGVlIHRoZSBkaXJlY3RvcnkgaXMgYWx3YXlzIHByZXNlbnQuXG4gICAqL1xuICBmdW5jdGlvbiBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIG91dERpciBpcyBub3Qgc2V0YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgdHJ5IHtcbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzXG4gICAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICAgIH1cblxuICAgICAgLy8gQWx3YXlzIGVuc3VyZSAuZ2l0aWdub3JlIGV4aXN0cyBpbiBvdXRwdXQgZGlyZWN0b3J5XG4gICAgICBjb25zdCBnaXRpZ25vcmVQYXRoID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgICB3cml0ZUZpbGVTeW5jKGdpdGlnbm9yZVBhdGgsIFwiKlxcblwiKTtcbiAgICAgIH1cbiAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIGZhaWxlZCB0byBlbnN1cmUgb3V0cHV0IGRpcmVjdG9yeTpgLCBlcnIpO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikge1xuICAgICAgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gbnVsbDtcbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiByZXNldCgpOiB2b2lkIHtcbiAgICBzdG9wcGluZyA9IGZhbHNlO1xuICAgIHRpbWVyID0gbnVsbDtcbiAgfVxuXG4gIHJldHVybiB7XG4gICAgbmFtZTogXCJhcHhcIixcbiAgICBhcHBseTogKCkgPT4gdHJ1ZSxcblxuICAgIGNvbmZpZ1Jlc29sdmVkKGNvbmZpZykge1xuICAgICAgb3V0RGlyID0gcmVzb2x2ZShjb25maWcucm9vdCwgY29uZmlnLmJ1aWxkLm91dERpcik7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYXMgc29vbiBhcyB3ZSBrbm93IHRoZSBvdXREaXJcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyB3aGVuIHNlcnZlciBzdGFydHNcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIGJ1aWxkIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIGlmIChzdGVwcy5sZW5ndGggPiAwKSB7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGhhbmRsZUhvdFVwZGF0ZShjdHgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uIGV2ZXJ5IEhNUiB1cGRhdGVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuXG4gICAgICAvLyBDaGVjayBpZiBmaWxlIHNob3VsZCBiZSBpZ25vcmVkXG4gICAgICBpZiAocmVzb2x2ZWRJZ25vcmVzLnNvbWUoKHBhdHRlcm4pID0+IGN0eC5maWxlLmluY2x1ZGVzKHBhdHRlcm4pKSkge1xuICAgICAgICByZXR1cm47XG4gICAgICB9XG5cbiAgICAgIC8vIERlYm91bmNlIHN0ZXAgZXhlY3V0aW9uIG9uIEhNUiB1cGRhdGVzXG4gICAgICBpZiAodGltZXIpIGNsZWFyVGltZW91dCh0aW1lcik7XG4gICAgICB0aW1lciA9IHNldFRpbWVvdXQoYXN5bmMgKCkgPT4ge1xuICAgICAgICB0aW1lciA9IG51bGw7XG5cbiAgICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYmVmb3JlIHJ1bm5pbmcgc3RlcHNcbiAgICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgICAgIGF3YWl0IHJ1bkFsbFN0ZXBzKCk7XG5cbiAgICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYWZ0ZXIgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgIH0sIDEwMCk7XG4gICAgfSxcblxuICAgIHdyaXRlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgYWZ0ZXIgZmlsZXMgYXJlIHdyaXR0ZW5cbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgIH0sXG5cbiAgICBjbG9zZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIG9uZSBmaW5hbCB0aW1lXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgIHN0b3AoKTtcbiAgICB9LFxuICB9O1xufVxuXG4vLyBEZWZhdWx0IGV4cG9ydCBmb3IgY29udmVuaWVuY2U6IGltcG9ydCBhcHggZnJvbSBcImFweFwiXG5leHBvcnQgZGVmYXVsdCBhcHg7XG4iCiAgXSwKICAibWFwcGluZ3MiOiAiO0FBQUE7QUFDQTtBQUVBO0FBQ0E7QUFDQTtBQXVDTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFTLENBQUMsTUFBTTtBQUVwQyxNQUFJO0FBQ0osTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUE0QixDQUFDO0FBRWpDLGlCQUFlLGFBQWEsQ0FBQyxRQUFtQztBQUM5RCxZQUFRLElBQUksMkJBQTJCLFFBQVE7QUFDL0MsNkJBQXlCO0FBQ3pCLGVBQVcsV0FBVyxVQUFVO0FBRTlCLGNBQVEsUUFBUSxXQUFXLE1BQU0sVUFBVSxNQUFNO0FBQ2pELFVBQUk7QUFBUSxnQkFBUSxJQUFJLE9BQU8sS0FBSyxDQUFDO0FBQ3JDLFVBQUk7QUFBUSxnQkFBUSxNQUFNLE9BQU8sS0FBSyxDQUFDO0FBQUEsSUFDekMsT0FBTztBQUVMLFlBQU0sT0FBTztBQUFBO0FBRWYsNkJBQXlCO0FBQUE7QUFHM0IsaUJBQWUsV0FBVyxHQUFrQjtBQUMxQyxlQUFXLFFBQVEsT0FBTztBQUN4QixVQUFJO0FBQVU7QUFDZCxZQUFNLFFBQVEsS0FBSyxJQUFJO0FBQ3ZCLFVBQUk7QUFDRixnQkFBUSxJQUFJLFNBQVMsS0FBSyxhQUFPO0FBQ2pDLGNBQU0sY0FBYyxLQUFLLE1BQU07QUFDL0IsZ0JBQVEsSUFBSSxTQUFTLEtBQUssZ0JBQVUsS0FBSyxJQUFJLElBQUksV0FBVztBQUFBLGVBQ3JELEtBQVA7QUFDQSxnQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsY0FBTTtBQUFBO0FBQUEsSUFFVjtBQUFBO0FBT0YsV0FBUyx3QkFBd0IsR0FBUztBQUN4QyxTQUFLLFFBQVE7QUFDWCxjQUFRLE1BQU0seUJBQXlCO0FBQ3ZDO0FBQUEsSUFDRjtBQUVBLFFBQUk7QUFFRixXQUFLLFdBQVcsTUFBTSxHQUFHO0FBQ3ZCLGtCQUFVLFFBQVEsRUFBRSxXQUFXLEtBQUssQ0FBQztBQUFBLE1BQ3ZDO0FBR0EsWUFBTSxnQkFBZ0IsS0FBSyxRQUFRLFlBQVk7QUFDL0MsV0FBSyxXQUFXLGFBQWEsR0FBRztBQUM5QixzQkFBYyxlQUFlLEtBQUs7QUFBQSxNQUNwQztBQUFBLGFBQ08sS0FBUDtBQUNBLGNBQVEsTUFBTSw0Q0FBNEMsR0FBRztBQUFBO0FBQUE7QUFJakUsV0FBUyxJQUFJLEdBQVM7QUFDcEIsUUFBSTtBQUFVO0FBQ2QsZUFBVztBQUNYLFFBQUksT0FBTztBQUNULG1CQUFhLEtBQUs7QUFDbEIsY0FBUTtBQUFBLElBQ1Y7QUFBQTtBQUdGLFdBQVMsS0FBSyxHQUFTO0FBQ3JCLGVBQVc7QUFDWCxZQUFRO0FBQUE7QUFHVixTQUFPO0FBQUEsSUFDTCxNQUFNO0FBQUEsSUFDTixPQUFPLE1BQU07QUFBQSxJQUViLGNBQWMsQ0FBQyxRQUFRO0FBQ3JCLGVBQVMsUUFBUSxPQUFPLE1BQU0sT0FBTyxNQUFNLE1BQU07QUFDakQsd0JBQWtCLE9BQU8sSUFBSSxDQUFDLFlBQzVCLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUNoQztBQUdBLFlBQU07QUFHTiwrQkFBeUI7QUFBQTtBQUFBLElBRzNCLGVBQWUsQ0FBQyxRQUFRO0FBQ3RCLGFBQU8sWUFBWSxLQUFLLFNBQVMsSUFBSTtBQUdyQywrQkFBeUI7QUFBQTtBQUFBLFNBR3JCLFdBQVUsR0FBRztBQUVqQiwrQkFBeUI7QUFFekIsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQiwrQkFBeUI7QUFHekIsVUFBSSxnQkFBZ0IsS0FBSyxDQUFDLFlBQVksSUFBSSxLQUFLLFNBQVMsT0FBTyxDQUFDLEdBQUc7QUFDakU7QUFBQSxNQUNGO0FBR0EsVUFBSTtBQUFPLHFCQUFhLEtBQUs7QUFDN0IsY0FBUSxXQUFXLFlBQVk7QUFDN0IsZ0JBQVE7QUFHUixpQ0FBeUI7QUFDekIsY0FBTSxZQUFZO0FBR2xCLGlDQUF5QjtBQUFBLFNBQ3hCLEdBQUc7QUFBQTtBQUFBLElBR1IsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQUE7QUFBQSxJQUczQixXQUFXLEdBQUc7QUFFWiwrQkFBeUI7QUFDekIsV0FBSztBQUFBO0FBQUEsRUFFVDtBQUFBO0FBckxGLElBQU0sWUFBWSxVQUFVLElBQUk7QUFZekIsSUFBTSxPQUFPLENBQUMsU0FBNkI7QUFPM0MsSUFBTSxVQUFVLENBQUMsV0FBbUIsZ0JBQWtDO0FBQUEsRUFDM0UsTUFBTTtBQUFBLEVBQ04sUUFBUSxzQkFBc0IsYUFBYTtBQUM3QztBQU1PLElBQU0sUUFBUSxDQUFDLFlBQW1DO0FBQUEsRUFDdkQsTUFBTTtBQUFBLEVBQ04sUUFBUSxNQUFNLFNBQVMsTUFBTTtBQUMvQjtBQTBKQSxJQUFlOyIsCiAgImRlYnVnSWQiOiAiRkREMDFDQjg4QUMxQTQ5MjY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
