"use client";

import { useState, useEffect } from "react";
import { Terminal, Copy, Check } from "lucide-react";
import { motion, AnimatePresence } from "framer-motion";

interface TerminalLine {
  text: string;
  delay: number;
  className?: string;
}

const terminalOutput: TerminalLine[] = [
  {
    text: "$ uvx --index https://databricks-solutions.github.io/apx/simple apx init charming-aurora",
    delay: 0,
    className: "text-blue-400",
  },
  {
    text: "Welcome to apx 🚀",
    delay: 200,
    className: "text-green-400 font-semibold",
  },
  { text: "", delay: 100 },
  {
    text: "What's the name of your app?: charming-aurora",
    delay: 200,
    className: "text-cyan-300",
  },
  {
    text: "Which template would you like to use?: essential",
    delay: 200,
    className: "text-cyan-300",
  },
  {
    text: "Available Databricks profiles: DEFAULT, dev",
    delay: 200,
    className: "text-gray-400 text-xs",
  },
  {
    text: "Which Databricks profile would you like to use? (leave empty to skip): dev",
    delay: 200,
    className: "text-cyan-300",
  },
  {
    text: "Would you like to set up AI assistant rules? yes",
    delay: 200,
    className: "text-cyan-300",
  },
  {
    text: "Which assistant would you like to use?: cursor",
    delay: 200,
    className: "text-cyan-300",
  },
  {
    text: "Which layout would you like to use?: sidebar",
    delay: 200,
    className: "text-cyan-300",
  },
  { text: "", delay: 200 },
  {
    text: "Initializing app charming-aurora in ~/projects/charming-aurora",
    delay: 200,
    className: "text-white",
  },
  { text: "", delay: 100 },
  {
    text: "✅ Project layout prepared (100ms)",
    delay: 200,
    className: "text-emerald-400",
  },
  {
    text: "✅ Git repository initialized (100ms)",
    delay: 200,
    className: "text-emerald-400",
  },
  {
    text: "✅ Assistant rules configured (100ms)",
    delay: 200,
    className: "text-emerald-400",
  },
  {
    text: "✅ Components added (100ms)",
    delay: 200,
    className: "text-emerald-400",
  },
  { text: "", delay: 300 },
  {
    text: "✨ Project charming-aurora initialized successfully!",
    delay: 200,
    className: "text-purple-400 font-semibold",
  },
  {
    text: "🚀 Run `cd ~/projects/charming-aurora && apx dev start` to get started!",
    delay: 200,
    className: "text-blue-400",
  },
  {
    text: "   (Dependencies will be installed automatically on first run)",
    delay: 200,
    className: "text-gray-400 text-sm",
  },
];

export function AnimatedTerminal() {
  const [visibleLines, setVisibleLines] = useState<number>(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [hasPlayed, setHasPlayed] = useState(false);
  const [copied, setCopied] = useState(false);

  const command =
    "uvx --index https://databricks-solutions.github.io/apx/simple apx init";

  // Auto-play on first load
  useEffect(() => {
    if (!hasPlayed) {
      setHasPlayed(true);
      handlePlay();
    }
  }, [hasPlayed]);

  useEffect(() => {
    if (!isPlaying) return;

    let currentDelay = 0;
    const timeouts: NodeJS.Timeout[] = [];

    terminalOutput.forEach((_, index) => {
      currentDelay += terminalOutput[index].delay;
      const timeout = setTimeout(() => {
        setVisibleLines(index + 1);
      }, currentDelay);
      timeouts.push(timeout);
    });

    return () => {
      timeouts.forEach((timeout) => clearTimeout(timeout));
    };
  }, [isPlaying]);

  const handlePlay = () => {
    setVisibleLines(0);
    setIsPlaying(true);
    setTimeout(
      () => {
        setIsPlaying(false);
      },
      terminalOutput.reduce((acc, line) => acc + line.delay, 0),
    );
  };

  const handleCopy = async () => {
    await navigator.clipboard.writeText(command);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="w-full max-w-4xl mx-auto">
      <motion.div
        initial={{ opacity: 0, y: 20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.5 }}
        className="rounded-2xl border border-fd-border bg-fd-card shadow-lg overflow-hidden"
      >
        {/* Terminal header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-white/10 bg-white/5">
          <div className="flex items-center gap-2">
            <Terminal className="w-4 h-4 text-green-400" />
            <span className="text-sm font-mono text-gray-300">apx init</span>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleCopy}
              className="flex items-center justify-center w-8 h-8 text-gray-300 hover:text-white transition-colors rounded-lg hover:bg-white/10"
              title="Copy command"
            >
              {copied ? (
                <Check className="w-4 h-4 text-green-400" />
              ) : (
                <Copy className="w-4 h-4" />
              )}
            </button>
          </div>
        </div>

        {/* Terminal content */}
        <div className="p-6 font-mono text-sm min-h-[500px] max-h-[600px] overflow-y-auto">
          <AnimatePresence mode="popLayout">
            {terminalOutput.slice(0, visibleLines).map((line, index) => (
              <motion.div
                key={index}
                initial={{ opacity: 0, x: -10 }}
                animate={{ opacity: 1, x: 0 }}
                transition={{ duration: 0.2 }}
                className={`mb-1 ${line.className || "text-gray-300"}`}
              >
                {line.text || "\u00A0"}
              </motion.div>
            ))}
          </AnimatePresence>

          {/* Blinking cursor */}
          {isPlaying && (
            <motion.span
              animate={{ opacity: [1, 0] }}
              transition={{ duration: 0.5, repeat: Infinity }}
              className="inline-block w-2 h-4 bg-green-400 ml-1"
            />
          )}
        </div>
      </motion.div>
    </div>
  );
}
