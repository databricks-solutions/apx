import { createMDX } from "fumadocs-mdx/next";

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
  output: "export",
  distDir: "./.pages",
  reactStrictMode: true,
  basePath: "/apx",
  assetPrefix: "/apx",
  turbopack: {
    root: process.cwd(), // Explicitly set to docs directory
  },
};

export default withMDX(config);
