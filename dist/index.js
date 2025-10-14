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
  let isServeMode = false;
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
  function reset() {
    stopping = false;
    timer = null;
  }
  return {
    name: "apx",
    apply: () => true,
    configResolved(config) {
      outDir = config.build.outDir;
      isServeMode = config.command === "serve";
      resolvedIgnores = ignore.map((pattern) => resolve(process.cwd(), pattern));
      reset();
      process.on("SIGINT", stop);
      process.on("SIGTERM", stop);
    },
    configureServer(server) {
      server.httpServer?.once("close", stop);
    },
    async buildStart() {
      if (isServeMode) {
        ensureGitignoreInOutDir();
      }
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
    writeBundle() {
      if (!isServeMode) {
        ensureGitignoreInOutDir();
      }
    },
    closeBundle() {
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

//# debugId=FAD0F4A8539A3C0864756E2164756E21
//# sourceMappingURL=data:application/json;base64,ewogICJ2ZXJzaW9uIjogMywKICAic291cmNlcyI6IFsic3JjL2FweC9wbHVnaW5zL2luZGV4LnRzIl0sCiAgInNvdXJjZXNDb250ZW50IjogWwogICAgImltcG9ydCB7IGV4aXN0c1N5bmMsIG1rZGlyU3luYywgd3JpdGVGaWxlU3luYyB9IGZyb20gXCJmc1wiO1xuaW1wb3J0IHsgam9pbiwgcmVzb2x2ZSB9IGZyb20gXCJwYXRoXCI7XG5pbXBvcnQgeyB0eXBlIFBsdWdpbiB9IGZyb20gXCJ2aXRlXCI7XG5pbXBvcnQgeyBleGVjIH0gZnJvbSBcImNoaWxkX3Byb2Nlc3NcIjtcbmltcG9ydCB7IHByb21pc2lmeSB9IGZyb20gXCJ1dGlsXCI7XG5cbmNvbnN0IGV4ZWNBc3luYyA9IHByb21pc2lmeShleGVjKTtcblxuZXhwb3J0IHR5cGUgU3RlcEFjdGlvbiA9IHN0cmluZyB8ICgoKSA9PiB2b2lkIHwgUHJvbWlzZTx2b2lkPik7XG5cbmV4cG9ydCB0eXBlIFN0ZXBTcGVjID0ge1xuICBuYW1lOiBzdHJpbmc7XG4gIGFjdGlvbjogU3RlcEFjdGlvbjtcbn07XG5cbmV4cG9ydCBjb25zdCBTdGVwID0gKHNwZWM6IFN0ZXBTcGVjKTogU3RlcFNwZWMgPT4gc3BlYztcblxuZXhwb3J0IGludGVyZmFjZSBBcHhQbHVnaW5PcHRpb25zIHtcbiAgc3RlcHM/OiBTdGVwU3BlY1tdO1xuICBpZ25vcmU/OiBzdHJpbmdbXTtcbn1cblxuZXhwb3J0IGZ1bmN0aW9uIGFweChvcHRpb25zOiBBcHhQbHVnaW5PcHRpb25zID0ge30pOiBQbHVnaW4ge1xuICBjb25zdCB7IHN0ZXBzID0gW10sIGlnbm9yZSA9IFtdIH0gPSBvcHRpb25zO1xuXG4gIGxldCBvdXREaXI6IHN0cmluZztcbiAgbGV0IHRpbWVyOiBOb2RlSlMuVGltZW91dCB8IG51bGwgPSBudWxsO1xuICBsZXQgc3RvcHBpbmcgPSBmYWxzZTtcbiAgbGV0IHJlc29sdmVkSWdub3Jlczogc3RyaW5nW10gPSBbXTtcbiAgbGV0IGlzU2VydmVNb2RlID0gZmFsc2U7XG5cbiAgYXN5bmMgZnVuY3Rpb24gZXhlY3V0ZUFjdGlvbihhY3Rpb246IFN0ZXBBY3Rpb24pOiBQcm9taXNlPHZvaWQ+IHtcbiAgICBpZiAodHlwZW9mIGFjdGlvbiA9PT0gXCJzdHJpbmdcIikge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBzaGVsbCBjb21tYW5kXG4gICAgICBjb25zdCB7IHN0ZG91dCwgc3RkZXJyIH0gPSBhd2FpdCBleGVjQXN5bmMoYWN0aW9uKTtcbiAgICAgIGlmIChzdGRvdXQpIGNvbnNvbGUubG9nKHN0ZG91dC50cmltKCkpO1xuICAgICAgaWYgKHN0ZGVycikgY29uc29sZS5lcnJvcihzdGRlcnIudHJpbSgpKTtcbiAgICB9IGVsc2Uge1xuICAgICAgLy8gRXhlY3V0ZSBhcyBmdW5jdGlvblxuICAgICAgYXdhaXQgYWN0aW9uKCk7XG4gICAgfVxuICB9XG5cbiAgYXN5bmMgZnVuY3Rpb24gcnVuQWxsU3RlcHMoKTogUHJvbWlzZTx2b2lkPiB7XG4gICAgZm9yIChjb25zdCBzdGVwIG9mIHN0ZXBzKSB7XG4gICAgICBpZiAoc3RvcHBpbmcpIGJyZWFrO1xuICAgICAgY29uc3Qgc3RhcnQgPSBEYXRlLm5vdygpO1xuICAgICAgdHJ5IHtcbiAgICAgICAgY29uc29sZS5sb2coYFthcHhdICR7c3RlcC5uYW1lfSDij7NgKTtcbiAgICAgICAgYXdhaXQgZXhlY3V0ZUFjdGlvbihzdGVwLmFjdGlvbik7XG4gICAgICAgIGNvbnNvbGUubG9nKGBbYXB4XSAke3N0ZXAubmFtZX0g4pyTICgke0RhdGUubm93KCkgLSBzdGFydH0gbXMpYCk7XG4gICAgICB9IGNhdGNoIChlcnIpIHtcbiAgICAgICAgY29uc29sZS5lcnJvcihgW2FweF0gJHtzdGVwLm5hbWV9IOKcl2AsIGVycik7XG4gICAgICAgIHRocm93IGVycjtcbiAgICAgIH1cbiAgICB9XG4gIH1cblxuICBmdW5jdGlvbiBlbnN1cmVHaXRpZ25vcmVJbk91dERpcigpOiB2b2lkIHtcbiAgICBpZiAoIW91dERpcikgcmV0dXJuO1xuXG4gICAgLy8gQ3JlYXRlIHRoZSBvdXRwdXQgZGlyZWN0b3J5IGlmIGl0IGRvZXNuJ3QgZXhpc3RcbiAgICBpZiAoIWV4aXN0c1N5bmMob3V0RGlyKSkge1xuICAgICAgbWtkaXJTeW5jKG91dERpciwgeyByZWN1cnNpdmU6IHRydWUgfSk7XG4gICAgfVxuXG4gICAgLy8gRW5zdXJlIC5naXRpZ25vcmUgZXhpc3RzIGluIG91dHB1dCBkaXJlY3RvcnlcbiAgICBjb25zdCBnaXRpZ25vcmVQYXRoID0gam9pbihvdXREaXIsIFwiLmdpdGlnbm9yZVwiKTtcbiAgICBpZiAoIWV4aXN0c1N5bmMoZ2l0aWdub3JlUGF0aCkpIHtcbiAgICAgIHdyaXRlRmlsZVN5bmMoZ2l0aWdub3JlUGF0aCwgXCIqXFxuXCIpO1xuICAgICAgY29uc29sZS5sb2coYFthcHhdIGVuc3VyZWQgJHtnaXRpZ25vcmVQYXRofWApO1xuICAgIH1cbiAgfVxuXG4gIGZ1bmN0aW9uIHN0b3AoKTogdm9pZCB7XG4gICAgaWYgKHN0b3BwaW5nKSByZXR1cm47XG4gICAgc3RvcHBpbmcgPSB0cnVlO1xuICAgIGlmICh0aW1lcikgY2xlYXJUaW1lb3V0KHRpbWVyKTtcbiAgICBjb25zb2xlLmxvZyhcIlthcHhdIHN0b3BwaW5nLi4uXCIpO1xuICB9XG5cbiAgZnVuY3Rpb24gcmVzZXQoKTogdm9pZCB7XG4gICAgc3RvcHBpbmcgPSBmYWxzZTtcbiAgICB0aW1lciA9IG51bGw7XG4gIH1cblxuICByZXR1cm4ge1xuICAgIG5hbWU6IFwiYXB4XCIsXG4gICAgYXBwbHk6ICgpID0+IHRydWUsXG5cbiAgICBjb25maWdSZXNvbHZlZChjb25maWcpIHtcbiAgICAgIG91dERpciA9IGNvbmZpZy5idWlsZC5vdXREaXI7XG4gICAgICBpc1NlcnZlTW9kZSA9IGNvbmZpZy5jb21tYW5kID09PSBcInNlcnZlXCI7XG4gICAgICByZXNvbHZlZElnbm9yZXMgPSBpZ25vcmUubWFwKChwYXR0ZXJuKSA9PlxuICAgICAgICByZXNvbHZlKHByb2Nlc3MuY3dkKCksIHBhdHRlcm4pLFxuICAgICAgKTtcblxuICAgICAgLy8gUmVzZXQgc3RhdGUgZm9yIG5ldyBidWlsZFxuICAgICAgcmVzZXQoKTtcblxuICAgICAgLy8gU2V0dXAgc2lnbmFsIGhhbmRsZXJzIGZvciBncmFjZWZ1bCBzaHV0ZG93blxuICAgICAgcHJvY2Vzcy5vbihcIlNJR0lOVFwiLCBzdG9wKTtcbiAgICAgIHByb2Nlc3Mub24oXCJTSUdURVJNXCIsIHN0b3ApO1xuICAgIH0sXG5cbiAgICBjb25maWd1cmVTZXJ2ZXIoc2VydmVyKSB7XG4gICAgICBzZXJ2ZXIuaHR0cFNlcnZlcj8ub25jZShcImNsb3NlXCIsIHN0b3ApO1xuICAgIH0sXG5cbiAgICBhc3luYyBidWlsZFN0YXJ0KCkge1xuICAgICAgLy8gT25seSBlbnN1cmUgZ2l0aWdub3JlIGluIHNlcnZlIG1vZGUgYXQgc3RhcnRcbiAgICAgIC8vIEluIGJ1aWxkIG1vZGUsIHdlJ2xsIGRvIGl0IGFmdGVyIGZpbGVzIGFyZSB3cml0dGVuXG4gICAgICBpZiAoaXNTZXJ2ZU1vZGUpIHtcbiAgICAgICAgZW5zdXJlR2l0aWdub3JlSW5PdXREaXIoKTtcbiAgICAgIH1cblxuICAgICAgaWYgKHN0ZXBzLmxlbmd0aCA+IDApIHtcbiAgICAgICAgYXdhaXQgcnVuQWxsU3RlcHMoKTtcbiAgICAgIH1cbiAgICB9LFxuXG4gICAgaGFuZGxlSG90VXBkYXRlKGN0eCkge1xuICAgICAgLy8gQ2hlY2sgaWYgZmlsZSBzaG91bGQgYmUgaWdub3JlZFxuICAgICAgaWYgKHJlc29sdmVkSWdub3Jlcy5zb21lKChwYXR0ZXJuKSA9PiBjdHguZmlsZS5pbmNsdWRlcyhwYXR0ZXJuKSkpIHtcbiAgICAgICAgcmV0dXJuO1xuICAgICAgfVxuXG4gICAgICAvLyBEZWJvdW5jZSBzdGVwIGV4ZWN1dGlvbiBvbiBITVIgdXBkYXRlc1xuICAgICAgaWYgKHRpbWVyKSBjbGVhclRpbWVvdXQodGltZXIpO1xuICAgICAgdGltZXIgPSBzZXRUaW1lb3V0KCgpID0+IHtcbiAgICAgICAgdGltZXIgPSBudWxsO1xuICAgICAgICB2b2lkIHJ1bkFsbFN0ZXBzKCk7XG4gICAgICB9LCAxMDApO1xuICAgIH0sXG5cbiAgICB3cml0ZUJ1bmRsZSgpIHtcbiAgICAgIC8vIEluIGJ1aWxkIG1vZGUsIGVuc3VyZSBnaXRpZ25vcmUgYWZ0ZXIgYWxsIGZpbGVzIGFyZSB3cml0dGVuXG4gICAgICBpZiAoIWlzU2VydmVNb2RlKSB7XG4gICAgICAgIGVuc3VyZUdpdGlnbm9yZUluT3V0RGlyKCk7XG4gICAgICB9XG4gICAgfSxcblxuICAgIGNsb3NlQnVuZGxlKCkge1xuICAgICAgc3RvcCgpO1xuICAgIH0sXG4gIH07XG59XG5cbi8vIERlZmF1bHQgZXhwb3J0IGZvciBjb252ZW5pZW5jZTogaW1wb3J0IGFweCBmcm9tIFwiYXB4XCJcbmV4cG9ydCBkZWZhdWx0IGFweDtcbiIKICBdLAogICJtYXBwaW5ncyI6ICI7QUFBQTtBQUNBO0FBRUE7QUFDQTtBQWtCTyxTQUFTLEdBQUcsQ0FBQyxVQUE0QixDQUFDLEdBQVc7QUFDMUQsVUFBUSxRQUFRLENBQUMsR0FBRyxTQUFTLENBQUMsTUFBTTtBQUVwQyxNQUFJO0FBQ0osTUFBSSxRQUErQjtBQUNuQyxNQUFJLFdBQVc7QUFDZixNQUFJLGtCQUE0QixDQUFDO0FBQ2pDLE1BQUksY0FBYztBQUVsQixpQkFBZSxhQUFhLENBQUMsUUFBbUM7QUFDOUQsZUFBVyxXQUFXLFVBQVU7QUFFOUIsY0FBUSxRQUFRLFdBQVcsTUFBTSxVQUFVLE1BQU07QUFDakQsVUFBSTtBQUFRLGdCQUFRLElBQUksT0FBTyxLQUFLLENBQUM7QUFDckMsVUFBSTtBQUFRLGdCQUFRLE1BQU0sT0FBTyxLQUFLLENBQUM7QUFBQSxJQUN6QyxPQUFPO0FBRUwsWUFBTSxPQUFPO0FBQUE7QUFBQTtBQUlqQixpQkFBZSxXQUFXLEdBQWtCO0FBQzFDLGVBQVcsUUFBUSxPQUFPO0FBQ3hCLFVBQUk7QUFBVTtBQUNkLFlBQU0sUUFBUSxLQUFLLElBQUk7QUFDdkIsVUFBSTtBQUNGLGdCQUFRLElBQUksU0FBUyxLQUFLLGFBQU87QUFDakMsY0FBTSxjQUFjLEtBQUssTUFBTTtBQUMvQixnQkFBUSxJQUFJLFNBQVMsS0FBSyxnQkFBVSxLQUFLLElBQUksSUFBSSxXQUFXO0FBQUEsZUFDckQsS0FBUDtBQUNBLGdCQUFRLE1BQU0sU0FBUyxLQUFLLGVBQVMsR0FBRztBQUN4QyxjQUFNO0FBQUE7QUFBQSxJQUVWO0FBQUE7QUFHRixXQUFTLHVCQUF1QixHQUFTO0FBQ3ZDLFNBQUs7QUFBUTtBQUdiLFNBQUssV0FBVyxNQUFNLEdBQUc7QUFDdkIsZ0JBQVUsUUFBUSxFQUFFLFdBQVcsS0FBSyxDQUFDO0FBQUEsSUFDdkM7QUFHQSxVQUFNLGdCQUFnQixLQUFLLFFBQVEsWUFBWTtBQUMvQyxTQUFLLFdBQVcsYUFBYSxHQUFHO0FBQzlCLG9CQUFjLGVBQWUsS0FBSztBQUNsQyxjQUFRLElBQUksaUJBQWlCLGVBQWU7QUFBQSxJQUM5QztBQUFBO0FBR0YsV0FBUyxJQUFJLEdBQVM7QUFDcEIsUUFBSTtBQUFVO0FBQ2QsZUFBVztBQUNYLFFBQUk7QUFBTyxtQkFBYSxLQUFLO0FBQzdCLFlBQVEsSUFBSSxtQkFBbUI7QUFBQTtBQUdqQyxXQUFTLEtBQUssR0FBUztBQUNyQixlQUFXO0FBQ1gsWUFBUTtBQUFBO0FBR1YsU0FBTztBQUFBLElBQ0wsTUFBTTtBQUFBLElBQ04sT0FBTyxNQUFNO0FBQUEsSUFFYixjQUFjLENBQUMsUUFBUTtBQUNyQixlQUFTLE9BQU8sTUFBTTtBQUN0QixvQkFBYyxPQUFPLFlBQVk7QUFDakMsd0JBQWtCLE9BQU8sSUFBSSxDQUFDLFlBQzVCLFFBQVEsUUFBUSxJQUFJLEdBQUcsT0FBTyxDQUNoQztBQUdBLFlBQU07QUFHTixjQUFRLEdBQUcsVUFBVSxJQUFJO0FBQ3pCLGNBQVEsR0FBRyxXQUFXLElBQUk7QUFBQTtBQUFBLElBRzVCLGVBQWUsQ0FBQyxRQUFRO0FBQ3RCLGFBQU8sWUFBWSxLQUFLLFNBQVMsSUFBSTtBQUFBO0FBQUEsU0FHakMsV0FBVSxHQUFHO0FBR2pCLFVBQUksYUFBYTtBQUNmLGdDQUF3QjtBQUFBLE1BQzFCO0FBRUEsVUFBSSxNQUFNLFNBQVMsR0FBRztBQUNwQixjQUFNLFlBQVk7QUFBQSxNQUNwQjtBQUFBO0FBQUEsSUFHRixlQUFlLENBQUMsS0FBSztBQUVuQixVQUFJLGdCQUFnQixLQUFLLENBQUMsWUFBWSxJQUFJLEtBQUssU0FBUyxPQUFPLENBQUMsR0FBRztBQUNqRTtBQUFBLE1BQ0Y7QUFHQSxVQUFJO0FBQU8scUJBQWEsS0FBSztBQUM3QixjQUFRLFdBQVcsTUFBTTtBQUN2QixnQkFBUTtBQUNSLFFBQUssWUFBWTtBQUFBLFNBQ2hCLEdBQUc7QUFBQTtBQUFBLElBR1IsV0FBVyxHQUFHO0FBRVosV0FBSyxhQUFhO0FBQ2hCLGdDQUF3QjtBQUFBLE1BQzFCO0FBQUE7QUFBQSxJQUdGLFdBQVcsR0FBRztBQUNaLFdBQUs7QUFBQTtBQUFBLEVBRVQ7QUFBQTtBQTNJRixJQUFNLFlBQVksVUFBVSxJQUFJO0FBU3pCLElBQU0sT0FBTyxDQUFDLFNBQTZCO0FBc0lsRCxJQUFlOyIsCiAgImRlYnVnSWQiOiAiRkFEMEY0QTg1MzlBM0MwODY0NzU2RTIxNjQ3NTZFMjEiLAogICJuYW1lcyI6IFtdCn0=
