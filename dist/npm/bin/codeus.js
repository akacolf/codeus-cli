#!/usr/bin/env node

const { spawnSync } = require("node:child_process")
const { existsSync } = require("node:fs")
const { join } = require("node:path")

const exe = process.platform === "win32" ? "codeus.exe" : "codeus"
const binary = join(__dirname, "..", "vendor", exe)

if (!existsSync(binary)) {
  console.error("Codeus binary is missing. Try reinstalling @quantumclub/codeus.")
  process.exit(1)
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" })
if (result.error) {
  console.error(result.error.message)
  process.exit(1)
}
process.exit(result.status ?? 0)
