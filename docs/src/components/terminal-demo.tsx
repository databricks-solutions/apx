"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

import {
  AnimatedSpan,
  Terminal,
  TypingAnimation,
} from "@/components/magic-ui/terminal";

const command =
  "curl -fsSL https://databricks-solutions.github.io/apx/install.sh | sh";

const baseLineClass = "break-words whitespace-pre-wrap text-xs md:text-sm";

const lines = [
  // --- install.sh output ---
  { text: "info: Detected platform: darwin aarch64", className: "text-cyan-300" },
  { text: "info: Install directory: /Users/dev/.local/bin", className: "text-cyan-300" },
  { text: "info: Fetching latest release...", className: "text-cyan-300" },
  { text: "info: Latest version: v0.3.0", className: "text-cyan-300" },
  { text: "info: Downloading apx-aarch64-darwin...", className: "text-cyan-300" },
  { text: "success: Installed apx to /Users/dev/.local/bin/apx", className: "text-green-400" },
  { text: "", className: "" },
  { text: "apx v0.3.0 installed successfully!", className: "text-green-400 font-bold" },
  { text: "", className: "" },
  // --- apx init ---
  { text: "$ apx init charming-aurora", className: "text-blue-400" },
  { text: "Welcome to apx 🚀", className: "text-green-400 font-semibold" },
  { text: "", className: "" },
  {
    text: "? Which addons would you like to enable? (space = toggle, enter = confirm, a = all) ›",
    className: "text-cyan-300",
  },
  { text: "  UI:", className: "text-white font-bold" },
  { text: "✔ ui — ⚡ Frontend with React, Vite, and TanStack Router", className: "text-emerald-400" },
  { text: "✔ sidebar — 📐 Sidebar navigation layout (includes UI)", className: "text-emerald-400" },
  { text: "  Backend:", className: "text-white font-bold" },
  { text: "⬚ lakebase — 🐘 Lakebase (Postgres) integration", className: "text-gray-400" },
  { text: "⬚ sql — 🗃️ SQL Warehouse connection and query API", className: "text-gray-400" },
  { text: "  AI Assistants:", className: "text-white font-bold" },
  { text: "✔ cursor — ✏️ Cursor IDE rules and MCP config", className: "text-emerald-400" },
  { text: "⬚ codex — 🧠 OpenAI Codex AGENTS.md file", className: "text-gray-400" },
  { text: "⬚ claude — 🤖 Claude Code project rules and MCP config", className: "text-gray-400" },
  { text: "⬚ vscode — 💻 VS Code instructions and MCP config", className: "text-gray-400" },
  { text: "", className: "" },
  { text: "Available Databricks profiles: DEFAULT, dev", className: "text-gray-400 text-xs" },
  { text: "Which Databricks profile would you like to use? (leave empty to skip): dev", className: "text-cyan-300" },
  { text: "", className: "" },
  { text: "✅ Project layout prepared (100ms)", className: "text-emerald-400" },
  { text: "✅ Git repository initialized (100ms)", className: "text-emerald-400" },
  { text: "✅ Assistant rules configured (100ms)", className: "text-emerald-400" },
  { text: "✅ Components added (100ms)", className: "text-emerald-400" },
  { text: "", className: "" },
  { text: "✨ Project charming-aurora initialized successfully!", className: "text-purple-400 font-semibold" },
  { text: "🚀 Run `cd ~/projects/charming-aurora && apx dev start` to get started!", className: "text-blue-400" },
];

export function TerminalDemo() {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    await navigator.clipboard.writeText(command);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="w-full max-w-4xl mx-auto">
      <div className="relative">
        <Terminal
          className="dark max-w-4xl h-[500px]"
          headerActions={
            <button
              type="button"
              onClick={handleCopy}
              className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-border bg-background/60 text-foreground transition-colors hover:bg-muted"
              title={copied ? "Copied" : "Copy command"}
            >
              {copied ? (
                <Check className="h-3.5 w-3.5 text-green-400" />
              ) : (
                <Copy className="h-3.5 w-3.5" />
              )}
            </button>
          }
        >
          <TypingAnimation
            className={`${baseLineClass} text-blue-400`}
            duration={10}
          >
            {`$ ${command}`}
          </TypingAnimation>
          {lines.map((line) => (
            <AnimatedSpan
              key={line.text}
              className={`${baseLineClass} ${line.className ?? ""}`}
            >
              {line.text}
            </AnimatedSpan>
          ))}
        </Terminal>
      </div>
    </div>
  );
}
