import Link from "next/link";
import Image from "next/image";
import {
  Server,
  Sparkles,
  ArrowRight,
  Github,
  Rocket,
  Bot,
  Code2,
  Package,
  Layers,
  Database,
  Palette,
  ChevronRight,
  Zap,
} from "lucide-react";
import { Python } from "@/components/icons/python";
import { React as ReactIcon } from "@/components/icons/react";
import { TypeScript } from "@/components/icons/typescript";
import { FastAPI } from "@/components/icons/fast-api";
import { shadcnui } from "@/components/icons/shadcn-ui";
import { Bun } from "@/components/icons/bun";
import { UV } from "@/components/icons/uv";
import { TerminalDemo } from "@/components/terminal-demo";

const techStack = [
  { name: "Python", icon: Python },
  { name: "FastAPI", icon: FastAPI },
  { name: "React", icon: ReactIcon },
  { name: "TypeScript", icon: TypeScript },
  { name: "shadcn/ui", icon: shadcnui },
  { name: "Bun", icon: Bun },
  { name: "uv", icon: UV },
];

export default function HomePage() {
  return (
    <main className="flex flex-1 flex-col">
      {/* Hero Section */}
      <section className="flex flex-col items-center justify-center px-6 py-16 md:py-24 text-center h-screen">
        {/* Logo */}
        <div className="mb-8">
          <Image
            src="/apx/logo.svg"
            alt="apx logo"
            width={128}
            height={128}
            unoptimized
            className="w-24 h-24 md:w-32 md:h-32 rounded-2xl shadow-lg"
          />
        </div>

        {/* Tagline with gradient */}
        <h1 className="text-4xl md:text-6xl font-bold mb-6 bg-gradient-to-r from-blue-400 via-purple-400 to-pink-400 bg-clip-text text-transparent leading-tight md:leading-tight pb-2">
          Build Databricks Apps
          <br />
          Lightning Fast
        </h1>

        {/* Description */}
        <p className="text-lg md:text-xl text-fd-muted-foreground max-w-2xl mb-4">
          Reliable, feature-full, human and LLM friendly development toolkit
        </p>

        {/* CTA Buttons */}
        <div className="flex flex-wrap gap-4 justify-center mb-16">
          <Link
            href="/docs"
            className="group inline-flex items-center justify-center gap-2 rounded-xl bg-gradient-to-r from-blue-500 to-purple-500 px-8 py-4 text-base font-semibold text-white shadow-lg shadow-purple-500/30 transition-all hover:shadow-xl hover:shadow-purple-500/40 hover:scale-105"
          >
            <Rocket className="w-5 h-5" />
            Get Started
            <ArrowRight className="w-5 h-5 group-hover:translate-x-1 transition-transform" />
          </Link>
          <Link
            href="https://github.com/databricks-solutions/apx"
            className="inline-flex items-center justify-center gap-2 rounded-xl border border-white/20 bg-white/5 backdrop-blur-sm px-8 py-4 text-base font-semibold text-fd-foreground shadow-lg transition-all hover:bg-white/10 hover:scale-105"
          >
            <Github className="w-5 h-5" />
            View on GitHub
          </Link>
        </div>
      </section>

      {/* Animated Terminal Demo */}
      <section className="px-6 py-16">
        <div className="max-w-6xl mx-auto">
          <div className="text-center mb-12">
            <h2 className="text-3xl md:text-4xl font-bold mb-4">Quickstart</h2>
          </div>
          <TerminalDemo />
        </div>
      </section>

      {/* Features Grid */}
      <section className="px-6 py-16 relative">
        <div className="max-w-5xl mx-auto">
          <div className="text-center mb-12">
            <h2 className="text-3xl md:text-4xl font-bold mb-4 flex items-center justify-center gap-3">
              <Package className="w-8 h-8 text-purple-400" />
              Everything you need
            </h2>
            <p className="text-fd-muted-foreground text-lg max-w-2xl mx-auto">
              apx provides a complete toolkit for building production-ready
              Databricks Apps
            </p>
          </div>

          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
            {/* CLI Reference */}
            <Link
              href="/docs/cli"
              className="group relative flex flex-col rounded-2xl border border-white/10 bg-gradient-to-br from-white/5 to-white/[0.02] p-6 backdrop-blur-sm transition-all hover:scale-[1.02] hover:border-blue-500/50 hover:shadow-xl hover:shadow-blue-500/20"
            >
              <div className="absolute inset-0 bg-gradient-to-br from-blue-500/0 to-blue-500/0 group-hover:from-blue-500/10 group-hover:to-transparent rounded-2xl transition-all" />
              <div className="relative">
                <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-gradient-to-br from-blue-500 to-cyan-500 text-white mb-4 shadow-lg shadow-blue-500/30">
                  <Code2 className="w-6 h-6" />
                </div>
                <h3 className="font-semibold text-lg text-fd-foreground mb-2 flex items-center gap-2">
                  CLI Reference
                  <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground group-hover:translate-x-1 transition-all" />
                </h3>
                <p className="text-sm text-fd-muted-foreground">
                  Initialize projects, build for production, and manage your
                  development workflow with powerful CLI commands.
                </p>
              </div>
            </Link>

            {/* MCP Reference */}
            <Link
              href="/docs/mcp"
              className="group relative flex flex-col rounded-2xl border border-white/10 bg-gradient-to-br from-white/5 to-white/[0.02] p-6 backdrop-blur-sm transition-all hover:scale-[1.02] hover:border-purple-500/50 hover:shadow-xl hover:shadow-purple-500/20"
            >
              <div className="absolute inset-0 bg-gradient-to-br from-purple-500/0 to-purple-500/0 group-hover:from-purple-500/10 group-hover:to-transparent rounded-2xl transition-all" />
              <div className="relative">
                <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-gradient-to-br from-purple-500 to-pink-500 text-white mb-4 shadow-lg shadow-purple-500/30">
                  <Server className="w-6 h-6" />
                </div>
                <h3 className="font-semibold text-lg text-fd-foreground mb-2 flex items-center gap-2">
                  MCP Reference
                  <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground group-hover:translate-x-1 transition-all" />
                </h3>
                <p className="text-sm text-fd-muted-foreground flex items-start gap-1">
                  <Bot className="w-4 h-4 mt-0.5 flex-shrink-0" />
                  Connect AI assistants to your development workflow with the
                  Model Context Protocol server.
                </p>
              </div>
            </Link>

            {/* APX Features */}
            <Link
              href="/docs/features"
              className="group relative flex flex-col rounded-2xl border border-white/10 bg-gradient-to-br from-white/5 to-white/[0.02] p-6 backdrop-blur-sm transition-all hover:scale-[1.02] hover:border-pink-500/50 hover:shadow-xl hover:shadow-pink-500/20"
            >
              <div className="absolute inset-0 bg-gradient-to-br from-pink-500/0 to-pink-500/0 group-hover:from-pink-500/10 group-hover:to-transparent rounded-2xl transition-all" />
              <div className="relative">
                <div className="flex items-center justify-center w-12 h-12 rounded-xl bg-gradient-to-br from-pink-500 to-orange-500 text-white mb-4 shadow-lg shadow-pink-500/30">
                  <Sparkles className="w-6 h-6" />
                </div>
                <h3 className="font-semibold text-lg text-fd-foreground mb-2 flex items-center gap-2">
                  Features
                  <ChevronRight className="w-4 h-4 ml-auto text-fd-muted-foreground group-hover:text-fd-foreground group-hover:translate-x-1 transition-all" />
                </h3>
                <p className="text-sm text-fd-muted-foreground flex items-start gap-1">
                  <Database className="w-4 h-4 mt-0.5 flex-shrink-0" />
                  Dev server, built-in components CLI, local dev database, and
                  more productivity features.
                </p>
              </div>
            </Link>
          </div>
        </div>
      </section>

      {/* Tech Stack */}
      <section className="px-6 py-16 relative">
        <div className="max-w-5xl mx-auto text-center">
          <div className="mb-12">
            <h2 className="text-3xl md:text-4xl font-bold mb-4 flex items-center justify-center gap-3">
              <Layers className="w-8 h-8 text-cyan-400" />
              On the shoulders of giants
            </h2>
            <p className="text-fd-muted-foreground text-lg max-w-2xl mx-auto flex items-center justify-center gap-2 text-balance">
              apx combines proven, reliable tools and frameworks to provide a
              complete toolkit for Databricks apps development
            </p>
          </div>

          <div className="flex flex-wrap justify-center gap-3">
            {techStack.map(({ name, icon: Icon }) => (
              <span
                key={name}
                className="group inline-flex items-center gap-2 rounded-full border border-white/20 bg-white/5 backdrop-blur-sm px-5 py-2.5 text-sm font-medium text-fd-foreground transition-all hover:scale-105 hover:border-white/40 hover:bg-white/10 hover:shadow-lg"
              >
                <Icon className="w-5 h-5 transition-transform group-hover:scale-110" />
                {name}
              </span>
            ))}
          </div>
        </div>
      </section>

      {/* Footer */}
      <footer className="relative px-6 py-12 border-t border-white/10 bg-gradient-to-b from-transparent to-black/20">
        <div className="max-w-5xl mx-auto">
          <div className="flex flex-col md:flex-row items-center justify-between gap-4">
            <div className="flex items-center gap-3">
              <Image
                src="/apx/logo.svg"
                alt="apx logo"
                width={32}
                height={32}
                unoptimized
                className="w-8 h-8 rounded-lg"
              />
              <span className="text-sm text-fd-muted-foreground">
                © 2026 Databricks Solutions
              </span>
            </div>
            <div className="flex items-center gap-6 text-sm">
              <Link
                href="https://github.com/databricks-solutions/apx/blob/main/LICENSE.md"
                className="text-fd-muted-foreground hover:text-fd-foreground transition-colors"
              >
                License
              </Link>
              <Link
                href="https://github.com/databricks-solutions/apx"
                className="text-fd-muted-foreground hover:text-fd-foreground transition-colors flex items-center gap-1"
              >
                <Github className="w-4 h-4" />
                GitHub
              </Link>
              <Link
                href="/docs"
                className="text-fd-muted-foreground hover:text-fd-foreground transition-colors"
              >
                Documentation
              </Link>
            </div>
          </div>
        </div>
      </footer>
    </main>
  );
}
