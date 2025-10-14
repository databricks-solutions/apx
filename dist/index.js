// src/apx/plugins/index.ts
import { existsSync, mkdirSync, writeFileSync } from "fs";
import { join, resolve } from "path";
import { exec } from "child_process";
import { promisify } from "util";
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
        console.log(`[apx] created output directory: ${outDir}`);
      }
      const gitignorePath = join(outDir, ".gitignore");
      if (!existsSync(gitignorePath)) {
        writeFileSync(gitignorePath, "*\n");
        console.log(`[apx] created ${gitignorePath}`);
      }
    } catch (err) {
      console.error(`[apx] failed to ensure output directory:`, err);
    }
  }
  function stop() {
    if (stopping)
      return;
    stopping = true;
    if (timer)
      clearTimeout(timer);
    console.log("[apx] stopping...");
  }
  function reset() {
    stopping = false;
    timer = null;
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = config.build.outDir;
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
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
var plugins_default = apx;
export {
  plugins_default as default,
  apx,
  Step
};

//# debugId=8E461FDA87A1515D64756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuZXhwb3J0IHR5cGUgU3RlcEFjdGlvbiA9IHN0cmluZyB8ICgoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPik7XG5cbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0ge1xuICBuYW1lOiBzdHJpbmc7XG4gIGFjdGlvbjogU3RlcEFjdGlvbjtcbn07XG5cbmV4cG9ydCBjb25zdCBTdGVwID0gKHNwZWM6IFN0ZXBTcGVjKTogU3RlcFNwZWMgPT4gc3BlYztcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGNvbnNvbGUubG9nKGBbYXB4XSBleGVjdXRpbmcgYWN0aW9uOiAke2FjdGlvbn1gKTtcbiAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICBpZiAodHlwZW9mIGFjdGlvbiA9PT0gXCJzdHJpbmdcIikge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBzaGVsbCBjb21tYW5kXG4gICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uKTtcbiAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgaWYgKHN0ZGVycikgY29uc29sZS5lcnJvcihzdGRlcnIudHJpbSgpKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgYXdhaXQgYWN0aW9uKCk7XG4gICAgfVxuICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gcnVuQWxsU3RlcHMoKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgZm9yIChjb25zdCBzdGVwIG9mIHN0ZXBzKSB7XG4gICAgICBpZiAoc3RvcHBpbmcpIGJyZWFrO1xuICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgdHJ5IHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyTICgke0RhdGUubm93KCkgLSBzdGFydH0gbXMpYCk7XG4gICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgIHRocm93IGVycjtcbiAgICAgIH1cbiAgICB9XG4gIH1cblxuICAvKipcbiAgICogRW5zdXJlcyB0aGUgb3V0cHV0IGRpcmVjdG9yeSBleGlzdHMgYW5kIGNvbnRhaW5zIGEgLmdpdGlnbm9yZSBmaWxlLlxuICAgKiBUaGlzIGlzIGNhbGxlZCBhdCBtdWx0aXBsZSBwb2ludHMgdG8gZ3VhcmFudGVlIHRoZSBkaXJlY3RvcnkgaXMgYWx3YXlzIHByZXNlbnQuXG4gICAqL1xuICBmdW5jdGlvbiBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTogdm9pZCB7XG4gICAgaWYgKCFvdXREaXIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIG91dERpciBpcyBub3Qgc2V0YCk7XG4gICAgICByZXR1cm47XG4gICAgfVxuXG4gICAgdHJ5IHtcbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgdGhlIG91dHB1dCBkaXJlY3RvcnkgZXhpc3RzXG4gICAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdIGNyZWF0ZWQgb3V0cHV0IGRpcmVjdG9yeTogJHtvdXREaXJ9YCk7XG4gICAgICB9XG5cbiAgICAgIC8vIEFsd2F5cyBlbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgICAgY29uc3QgZ2l0aWdub3JlUGF0aCA9IGpvaW4ob3V0RGlyLCBcIi5naXRpZ25vcmVcIik7XG4gICAgICBpZiAoIWV4aXN0c1N5bmMoZ2l0aWdub3JlUGF0aCkpIHtcbiAgICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSBjcmVhdGVkICR7Z2l0aWdub3JlUGF0aH1gKTtcbiAgICAgIH1cbiAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgIGNvbnNvbGUuZXJyb3IoYFthcHhdIGZhaWxlZCB0byBlbnN1cmUgb3V0cHV0IGRpcmVjdG9yeTpgLCBlcnIpO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICBjb25zb2xlLmxvZyhcIlthcHhdIHN0b3BwaW5nLi4uXCIpO1xuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG5cbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IGNvbmZpZy5idWlsZC5vdXREaXI7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gU2V0dXAgc2lnbmFsIGhhbmRsZXJzIGZvciBncmFjZWZ1bCBzaHV0ZG93blxuICAgICAgcHJvY2Vzcy5vbihcIlNJR0lOVFwiLCBzdG9wKTtcbiAgICAgIHByb2Nlc3Mub24oXCJTSUdURVJNXCIsIHN0b3ApO1xuXG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhcyBzb29uIGFzIHdlIGtub3cgdGhlIG91dERpclxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNvbmZpZ3VyZVNlcnZlcihzZXJ2ZXIpIHtcbiAgICAgIHNlcnZlci5odHRwU2VydmVyPy5vbmNlKFwiY2xvc2VcIiwgc3RvcCk7XG5cbiAgICAgIC8vIEVuc3VyZSBkaXJlY3RvcnkgZXhpc3RzIHdoZW4gc2VydmVyIHN0YXJ0c1xuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGFzeW5jIGJ1aWxkU3RhcnQoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgYnVpbGQgc3RhcnRzXG4gICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb24gZXZlcnkgSE1SIHVwZGF0ZVxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG5cbiAgICAgIC8vIENoZWNrIGlmIGZpbGUgc2hvdWxkIGJlIGlnbm9yZWRcbiAgICAgIGlmIChyZXNvbHZlZElnbm9yZXMuc29tZSgocGF0dGVybikgPT4gY3R4LmZpbGUuaW5jbHVkZXMocGF0dGVybikpKSB7XG4gICAgICAgIHJldHVybjtcbiAgICAgIH1cblxuICAgICAgLy8gRGVib3VuY2Ugc3RlcCBleGVjdXRpb24gb24gSE1SIHVwZGF0ZXNcbiAgICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICAgIHRpbWVyID0gc2V0VGltZW91dChhc3luYyAoKSA9PiB7XG4gICAgICAgIHRpbWVyID0gbnVsbDtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBiZWZvcmUgcnVubmluZyBzdGVwc1xuICAgICAgICBlbnN1cmVPdXREaXJBbmRHaXRpZ25vcmUoKTtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcblxuICAgICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBydW5uaW5nIHN0ZXBzXG4gICAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgfSwgMTAwKTtcbiAgICB9LFxuXG4gICAgd3JpdGVCdW5kbGUoKSB7XG4gICAgICAvLyBFbnN1cmUgZGlyZWN0b3J5IGV4aXN0cyBhZnRlciBmaWxlcyBhcmUgd3JpdHRlblxuICAgICAgZW5zdXJlT3V0RGlyQW5kR2l0aWdub3JlKCk7XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgLy8gRW5zdXJlIGRpcmVjdG9yeSBleGlzdHMgb25lIGZpbmFsIHRpbWVcbiAgICAgIGVuc3VyZU91dERpckFuZEdpdGlnbm9yZSgpO1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDtcbiIKICBdLAogICJtYXBwaW5ncyI6ICI7QUFBQTtBQUNBO0FBRUE7QUFDQTtBQWtCTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFTLENBQUMsTUFBTTtBQUVwQyxNQUFJO0FBQ0osTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUE0QixDQUFDO0FBRWpDLGlCQUFlLGFBQWEsQ0FBQyxRQUFtQztBQUM5RCxZQUFRLElBQUksMkJBQTJCLFFBQVE7QUFDL0MsNkJBQXlCO0FBQ3pCLGVBQVcsV0FBVyxVQUFVO0FBRTlCLGNBQVEsUUFBUSxXQUFXLE1BQU0sVUFBVSxNQUFNO0FBQ2pELFVBQUk7QUFBUSxnQkFBUSxJQUFJLE9BQU8sS0FBSyxDQUFDO0FBQ3JDLFVBQUk7QUFBUSxnQkFBUSxNQUFNLE9BQU8sS0FBSyxDQUFDO0FBQUEsSUFDekMsT0FBTztBQUVMLFlBQU0sT0FBTztBQUFBO0FBRWYsNkJBQXlCO0FBQUE7QUFHM0IsaUJBQWUsV0FBVyxHQUFrQjtBQUMxQyxlQUFXLFFBQVEsT0FBTztBQUN4QixVQUFJO0FBQVU7QUFDZCxZQUFNLFFBQVEsS0FBSyxJQUFJO0FBQ3ZCLFVBQUk7QUFDRixnQkFBUSxJQUFJLFNBQVMsS0FBSyxhQUFPO0FBQ2pDLGNBQU0sY0FBYyxLQUFLLE1BQU07QUFDL0IsZ0JBQVEsSUFBSSxTQUFTLEtBQUssZ0JBQVUsS0FBSyxJQUFJLElBQUksV0FBVztBQUFBLGVBQ3JELEtBQVA7QUFDQSxnQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsY0FBTTtBQUFBO0FBQUEsSUFFVjtBQUFBO0FBT0YsV0FBUyx3QkFBd0IsR0FBUztBQUN4QyxTQUFLLFFBQVE7QUFDWCxjQUFRLE1BQU0seUJBQXlCO0FBQ3ZDO0FBQUEsSUFDRjtBQUVBLFFBQUk7QUFFRixXQUFLLFdBQVcsTUFBTSxHQUFHO0FBQ3ZCLGtCQUFVLFFBQVEsRUFBRSxXQUFXLEtBQUssQ0FBQztBQUNyQyxnQkFBUSxJQUFJLG1DQUFtQyxRQUFRO0FBQUEsTUFDekQ7QUFHQSxZQUFNLGdCQUFnQixLQUFLLFFBQVEsWUFBWTtBQUMvQyxXQUFLLFdBQVcsYUFBYSxHQUFHO0FBQzlCLHNCQUFjLGVBQWUsS0FBSztBQUNsQyxnQkFBUSxJQUFJLGlCQUFpQixlQUFlO0FBQUEsTUFDOUM7QUFBQSxhQUNPLEtBQVA7QUFDQSxjQUFRLE1BQU0sNENBQTRDLEdBQUc7QUFBQTtBQUFBO0FBSWpFLFdBQVMsSUFBSSxHQUFTO0FBQ3BCLFFBQUk7QUFBVTtBQUNkLGVBQVc7QUFDWCxRQUFJO0FBQU8sbUJBQWEsS0FBSztBQUM3QixZQUFRLElBQUksbUJBQW1CO0FBQUE7QUFHakMsV0FBUyxLQUFLLEdBQVM7QUFDckIsZUFBVztBQUNYLFlBQVE7QUFBQTtBQUdWLFNBQU87QUFBQSxJQUNMLE1BQU07QUFBQSxJQUNOLE9BQU8sTUFBTTtBQUFBLElBRWIsY0FBYyxDQUFDLFFBQVE7QUFDckIsZUFBUyxPQUFPLE1BQU07QUFDdEIsd0JBQWtCLE9BQU8sSUFBSSxDQUFDLFlBQzVCLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUNoQztBQUdBLFlBQU07QUFHTixjQUFRLEdBQUcsVUFBVSxJQUFJO0FBQ3pCLGNBQVEsR0FBRyxXQUFXLElBQUk7QUFHMUIsK0JBQXlCO0FBQUE7QUFBQSxJQUczQixlQUFlLENBQUMsUUFBUTtBQUN0QixhQUFPLFlBQVksS0FBSyxTQUFTLElBQUk7QUFHckMsK0JBQXlCO0FBQUE7QUFBQSxTQUdyQixXQUFVLEdBQUc7QUFFakIsK0JBQXlCO0FBRXpCLFVBQUksTUFBTSxTQUFTLEdBQUc7QUFDcEIsY0FBTSxZQUFZO0FBQUEsTUFDcEI7QUFBQTtBQUFBLElBR0YsZUFBZSxDQUFDLEtBQUs7QUFFbkIsK0JBQXlCO0FBR3pCLFVBQUksZ0JBQWdCLEtBQUssQ0FBQyxZQUFZLElBQUksS0FBSyxTQUFTLE9BQU8sQ0FBQyxHQUFHO0FBQ2pFO0FBQUEsTUFDRjtBQUdBLFVBQUk7QUFBTyxxQkFBYSxLQUFLO0FBQzdCLGNBQVEsV0FBVyxZQUFZO0FBQzdCLGdCQUFRO0FBR1IsaUNBQXlCO0FBQ3pCLGNBQU0sWUFBWTtBQUdsQixpQ0FBeUI7QUFBQSxTQUN4QixHQUFHO0FBQUE7QUFBQSxJQUdSLFdBQVcsR0FBRztBQUVaLCtCQUF5QjtBQUFBO0FBQUEsSUFHM0IsV0FBVyxHQUFHO0FBRVosK0JBQXlCO0FBQ3pCLFdBQUs7QUFBQTtBQUFBLEVBRVQ7QUFBQTtBQXBLRixJQUFNLFlBQVksVUFBVSxJQUFJO0FBU3pCLElBQU0sT0FBTyxDQUFDLFNBQTZCO0FBK0psRCxJQUFlOyIsCiAgImRlYnVnSWQiOiAiOEU0NjFGREE4N0ExNTE1RDY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
