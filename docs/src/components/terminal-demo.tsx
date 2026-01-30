"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";

import {
  AnimatedSpan,
  Terminal,
  TypingAnimation,
} from "@/components/magic-ui/terminal";

const command =
  "uvx --index https://databricks-solutions.github.io/apx/simple apx init charming-aurora";

const baseLineClass = "break-words whitespace-pre-wrap text-xs md:text-sm";

const lines = [
  { text: "Welcome to apx 🚀", className: "text-green-400 font-semibold" },
  {
    text: "What's the name of your app?: charming-aurora",
    className: "text-cyan-300",
  },
  {
    text: "Which template would you like to use?: essential",
    className: "text-cyan-300",
  },
  {
    text: "Available Databricks profiles: DEFAULT, dev",
    className: "text-gray-400 text-xs",
  },
  {
    text: "Which Databricks profile would you like to use? (leave empty to skip): dev",
    className: "text-cyan-300",
  },
  {
    text: "Would you like to set up AI assistant rules? yes",
    className: "text-cyan-300",
  },
  {
    text: "Which assistant would you like to use?: cursor",
    className: "text-cyan-300",
  },
  {
    text: "Which layout would you like to use?: sidebar",
    className: "text-cyan-300",
  },
  { text: "✅ Project layout prepared (100ms)", className: "text-emerald-400" },
  {
    text: "✅ Git repository initialized (100ms)",
    className: "text-emerald-400",
  },
  {
    text: "✅ Assistant rules configured (100ms)",
    className: "text-emerald-400",
  },
  {
    text: "✨ Project charming-aurora initialized successfully!",
    className: "text-purple-400 font-semibold",
  },
  {
    text: "🚀 Run `cd ~/projects/charming-aurora && apx dev start` to get started!",
    className: "text-blue-400",
  },
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
          className="dark max-w-4xl min-h-[420px]"
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
