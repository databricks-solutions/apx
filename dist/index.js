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
    if (typeof action === "string") {
      const { stdout, stderr } = await execAsync(action);
      if (stdout)
        console.log(stdout.trim());
      if (stderr)
        console.error(stderr.trim());
    } else {
      await action();
    }
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
  function ensureGitignoreInOutDir() {
    if (!outDir)
      return;
    if (!existsSync(outDir)) {
      mkdirSync(outDir, { recursive: true });
    }
    const gitignorePath = join(outDir, ".gitignore");
    if (!existsSync(gitignorePath)) {
      writeFileSync(gitignorePath, "*\n");
      console.log(`[apx] ensured ${gitignorePath}`);
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
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = config.build.outDir;
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
    },
    async buildStart() {
      ensureGitignoreInOutDir();
      if (steps.length > 0) {
        await runAllSteps();
      }
    },
    handleHotUpdate(ctx) {
      if (resolvedIgnores.some((pattern) => ctx.file.includes(pattern))) {
        return;
      }
      if (timer)
        clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        runAllSteps();
      }, 100);
    },
    closeBundle() {
      ensureGitignoreInOutDir();
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

//# debugId=EA364735EFE10EC464756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuZXhwb3J0IHR5cGUgU3RlcEFjdGlvbiA9IHN0cmluZyB8ICgoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPik7XG5cbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0ge1xuICBuYW1lOiBzdHJpbmc7XG4gIGFjdGlvbjogU3RlcEFjdGlvbjtcbn07XG5cbmV4cG9ydCBjb25zdCBTdGVwID0gKHNwZWM6IFN0ZXBTcGVjKTogU3RlcFNwZWMgPT4gc3BlYztcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcblxuICBhc3luYyBmdW5jdGlvbiBleGVjdXRlQWN0aW9uKGFjdGlvbjogU3RlcEFjdGlvbik6IFByb21pc2U8dm9pZD4ge1xuICAgIGlmICh0eXBlb2YgYWN0aW9uID09PSBcInN0cmluZ1wiKSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIHNoZWxsIGNvbW1hbmRcbiAgICAgIGNvbnN0IHsgc3Rkb3V0LCBzdGRlcnIgfSA9IGF3YWl0IGV4ZWNBc3luYyhhY3Rpb24pO1xuICAgICAgaWYgKHN0ZG91dCkgY29uc29sZS5sb2coc3Rkb3V0LnRyaW0oKSk7XG4gICAgICBpZiAoc3RkZXJyKSBjb25zb2xlLmVycm9yKHN0ZGVyci50cmltKCkpO1xuICAgIH0gZWxzZSB7XG4gICAgICAvLyBFeGVjdXRlIGFzIGZ1bmN0aW9uXG4gICAgICBhd2FpdCBhY3Rpb24oKTtcbiAgICB9XG4gIH1cblxuICBhc3luYyBmdW5jdGlvbiBydW5BbGxTdGVwcygpOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBmb3IgKGNvbnN0IHN0ZXAgb2Ygc3RlcHMpIHtcbiAgICAgIGlmIChzdG9wcGluZykgYnJlYWs7XG4gICAgICBjb25zdCBzdGFydCA9IERhdGUubm93KCk7XG4gICAgICB0cnkge1xuICAgICAgICBjb25zb2xlLmxvZyhgW2FweF0gJHtzdGVwLm5hbWV9IOKPs2ApO1xuICAgICAgICBhd2FpdCBleGVjdXRlQWN0aW9uKHN0ZXAuYWN0aW9uKTtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDinJMgKCR7RGF0ZS5ub3coKSAtIHN0YXJ0fSBtcylgKTtcbiAgICAgIH0gY2F0Y2ggKGVycikge1xuICAgICAgICBjb25zb2xlLmVycm9yKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyXYCwgZXJyKTtcbiAgICAgICAgdGhyb3cgZXJyO1xuICAgICAgfVxuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIGVuc3VyZUdpdGlnbm9yZUluT3V0RGlyKCk6IHZvaWQge1xuICAgIGlmICghb3V0RGlyKSByZXR1cm47XG5cbiAgICAvLyBDcmVhdGUgdGhlIG91dHB1dCBkaXJlY3RvcnkgaWYgaXQgZG9lc24ndCBleGlzdFxuICAgIGlmICghZXhpc3RzU3luYyhvdXREaXIpKSB7XG4gICAgICBta2RpclN5bmMob3V0RGlyLCB7IHJlY3Vyc2l2ZTogdHJ1ZSB9KTtcbiAgICB9XG5cbiAgICAvLyBFbnN1cmUgLmdpdGlnbm9yZSBleGlzdHMgaW4gb3V0cHV0IGRpcmVjdG9yeVxuICAgIGNvbnN0IGdpdGlnbm9yZVBhdGggPSBqb2luKG91dERpciwgXCIuZ2l0aWdub3JlXCIpO1xuICAgIGlmICghZXhpc3RzU3luYyhnaXRpZ25vcmVQYXRoKSkge1xuICAgICAgd3JpdGVGaWxlU3luYyhnaXRpZ25vcmVQYXRoLCBcIipcXG5cIik7XG4gICAgICBjb25zb2xlLmxvZyhgW2FweF0gZW5zdXJlZCAke2dpdGlnbm9yZVBhdGh9YCk7XG4gICAgfVxuICB9XG5cbiAgZnVuY3Rpb24gc3RvcCgpOiB2b2lkIHtcbiAgICBpZiAoc3RvcHBpbmcpIHJldHVybjtcbiAgICBzdG9wcGluZyA9IHRydWU7XG4gICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgIGNvbnNvbGUubG9nKFwiW2FweF0gc3RvcHBpbmcuLi5cIik7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG5cbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IGNvbmZpZy5idWlsZC5vdXREaXI7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gU2V0dXAgc2lnbmFsIGhhbmRsZXJzIGZvciBncmFjZWZ1bCBzaHV0ZG93blxuICAgICAgcHJvY2Vzcy5vbihcIlNJR0lOVFwiLCBzdG9wKTtcbiAgICAgIHByb2Nlc3Mub24oXCJTSUdURVJNXCIsIHN0b3ApO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgZW5zdXJlR2l0aWdub3JlSW5PdXREaXIoKTtcblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gQ2hlY2sgaWYgZmlsZSBzaG91bGQgYmUgaWdub3JlZFxuICAgICAgaWYgKHJlc29sdmVkSWdub3Jlcy5zb21lKChwYXR0ZXJuKSA9PiBjdHguZmlsZS5pbmNsdWRlcyhwYXR0ZXJuKSkpIHtcbiAgICAgICAgcmV0dXJuO1xuICAgICAgfVxuXG4gICAgICAvLyBEZWJvdW5jZSBzdGVwIGV4ZWN1dGlvbiBvbiBITVIgdXBkYXRlc1xuICAgICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KCgpID0+IHtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuICAgICAgICB2b2lkIHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9LCAxMDApO1xuICAgIH0sXG5cbiAgICBjbG9zZUJ1bmRsZSgpIHtcbiAgICAgIGVuc3VyZUdpdGlnbm9yZUluT3V0RGlyKCk7XG4gICAgICBzdG9wKCk7XG4gICAgfSxcbiAgfTtcbn1cblxuLy8gRGVmYXVsdCBleHBvcnQgZm9yIGNvbnZlbmllbmNlOiBpbXBvcnQgYXB4IGZyb20gXCJhcHhcIlxuZXhwb3J0IGRlZmF1bHQgYXB4O1xuIgogIF0sCiAgIm1hcHBpbmdzIjogIjtBQUFBO0FBQ0E7QUFFQTtBQUNBO0FBa0JPLFNBQVMsR0FBRyxDQUFDLFVBQTRCLENBQUMsR0FBVztBQUMxRCxVQUFRLFFBQVEsQ0FBQyxHQUFHLFNBQVMsQ0FBQyxNQUFNO0FBRXBDLE1BQUk7QUFDSixNQUFJLFFBQStCO0FBQ25DLE1BQUksV0FBVztBQUNmLE1BQUksa0JBQTRCLENBQUM7QUFFakMsaUJBQWUsYUFBYSxDQUFDLFFBQW1DO0FBQzlELGVBQVcsV0FBVyxVQUFVO0FBRTlCLGNBQVEsUUFBUSxXQUFXLE1BQU0sVUFBVSxNQUFNO0FBQ2pELFVBQUk7QUFBUSxnQkFBUSxJQUFJLE9BQU8sS0FBSyxDQUFDO0FBQ3JDLFVBQUk7QUFBUSxnQkFBUSxNQUFNLE9BQU8sS0FBSyxDQUFDO0FBQUEsSUFDekMsT0FBTztBQUVMLFlBQU0sT0FBTztBQUFBO0FBQUE7QUFJakIsaUJBQWUsV0FBVyxHQUFrQjtBQUMxQyxlQUFXLFFBQVEsT0FBTztBQUN4QixVQUFJO0FBQVU7QUFDZCxZQUFNLFFBQVEsS0FBSyxJQUFJO0FBQ3ZCLFVBQUk7QUFDRixnQkFBUSxJQUFJLFNBQVMsS0FBSyxhQUFPO0FBQ2pDLGNBQU0sY0FBYyxLQUFLLE1BQU07QUFDL0IsZ0JBQVEsSUFBSSxTQUFTLEtBQUssZ0JBQVUsS0FBSyxJQUFJLElBQUksV0FBVztBQUFBLGVBQ3JELEtBQVA7QUFDQSxnQkFBUSxNQUFNLFNBQVMsS0FBSyxlQUFTLEdBQUc7QUFDeEMsY0FBTTtBQUFBO0FBQUEsSUFFVjtBQUFBO0FBR0YsV0FBUyx1QkFBdUIsR0FBUztBQUN2QyxTQUFLO0FBQVE7QUFHYixTQUFLLFdBQVcsTUFBTSxHQUFHO0FBQ3ZCLGdCQUFVLFFBQVEsRUFBRSxXQUFXLEtBQUssQ0FBQztBQUFBLElBQ3ZDO0FBR0EsVUFBTSxnQkFBZ0IsS0FBSyxRQUFRLFlBQVk7QUFDL0MsU0FBSyxXQUFXLGFBQWEsR0FBRztBQUM5QixvQkFBYyxlQUFlLEtBQUs7QUFDbEMsY0FBUSxJQUFJLGlCQUFpQixlQUFlO0FBQUEsSUFDOUM7QUFBQTtBQUdGLFdBQVMsSUFBSSxHQUFTO0FBQ3BCLFFBQUk7QUFBVTtBQUNkLGVBQVc7QUFDWCxRQUFJO0FBQU8sbUJBQWEsS0FBSztBQUM3QixZQUFRLElBQUksbUJBQW1CO0FBQUE7QUFHakMsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFFYixjQUFjLENBQUMsUUFBUTtBQUNyQixlQUFTLE9BQU8sTUFBTTtBQUN0Qix3QkFBa0IsT0FBTyxJQUFJLENBQUMsWUFDNUIsUUFBUSxRQUFRLElBQUksR0FBRyxPQUFPLENBQ2hDO0FBR0EsY0FBUSxHQUFHLFVBQVUsSUFBSTtBQUN6QixjQUFRLEdBQUcsV0FBVyxJQUFJO0FBQUE7QUFBQSxJQUc1QixlQUFlLENBQUMsUUFBUTtBQUN0QixhQUFPLFlBQVksS0FBSyxTQUFTLElBQUk7QUFBQTtBQUFBLFNBR2pDLFdBQVUsR0FBRztBQUNqQiw4QkFBd0I7QUFFeEIsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQixVQUFJLGdCQUFnQixLQUFLLENBQUMsWUFBWSxJQUFJLEtBQUssU0FBUyxPQUFPLENBQUMsR0FBRztBQUNqRTtBQUFBLE1BQ0Y7QUFHQSxVQUFJO0FBQU8scUJBQWEsS0FBSztBQUM3QixjQUFRLFdBQVcsTUFBTTtBQUN2QixnQkFBUTtBQUNSLFFBQUssWUFBWTtBQUFBLFNBQ2hCLEdBQUc7QUFBQTtBQUFBLElBR1IsV0FBVyxHQUFHO0FBQ1osOEJBQXdCO0FBQ3hCLFdBQUs7QUFBQTtBQUFBLEVBRVQ7QUFBQTtBQXZIRixJQUFNLFlBQVksVUFBVSxJQUFJO0FBU3pCLElBQU0sT0FBTyxDQUFDLFNBQTZCO0FBa0hsRCxJQUFlOyIsCiAgImRlYnVnSWQiOiAiRUEzNjQ3MzVFRkUxMEVDNDY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
