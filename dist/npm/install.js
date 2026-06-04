const { createHash } = require("node:crypto")
const { createWriteStream, mkdirSync, readFileSync, rmSync, chmodSync, copyFileSync } = require("node:fs")
const { tmpdir } = require("node:os")
const { basename, join } = require("node:path")
const { spawnSync } = require("node:child_process")
const https = require("node:https")

const repo = process.env.CODEUS_REPO || "atomicfield/codeus-cli"
const version = process.env.CODEUS_VERSION || require("./package.json").version
const base =
  version === "0.0.0-dev" || version === "latest"
    ? `https://github.com/${repo}/releases/latest/download`
    : `https://github.com/${repo}/releases/download/v${version}`

function target() {
  const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : null
  if (!arch) throw new Error(`unsupported architecture: ${process.arch}`)

  if (process.platform === "darwin") return `${arch}-apple-darwin`
  if (process.platform === "linux") return `${arch}-unknown-linux-musl`
  if (process.platform === "win32") return `${arch}-pc-windows-msvc`
  throw new Error(`unsupported platform: ${process.platform}`)
}

function archiveName(targetTriple) {
  return `codeus-${targetTriple}.${process.platform === "win32" ? "zip" : "tar.gz"}`
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    https
      .get(url, (res) => {
        if ([301, 302, 303, 307, 308].includes(res.statusCode) && res.headers.location) {
          download(res.headers.location, dest).then(resolve, reject)
          return
        }
        if (res.statusCode !== 200) {
          reject(new Error(`download failed (${res.statusCode}): ${url}`))
          return
        }
        const file = createWriteStream(dest)
        res.pipe(file)
        file.on("finish", () => file.close(resolve))
        file.on("error", reject)
      })
      .on("error", reject)
  })
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex")
}

function verify(archive, checksums, asset) {
  const line = readFileSync(checksums, "utf8")
    .split(/\r?\n/)
    .find((entry) => entry.endsWith(`  ${asset}`))
  if (!line) throw new Error(`checksum for ${asset} not found`)
  const expected = line.split(/\s+/)[0].toLowerCase()
  const actual = sha256(archive).toLowerCase()
  if (actual !== expected) throw new Error(`checksum mismatch for ${asset}`)
}

function unpack(archive, dest) {
  mkdirSync(dest, { recursive: true })
  if (process.platform === "win32") {
    const ps = spawnSync("powershell", ["-NoProfile", "-Command", `Expand-Archive -Force '${archive}' '${dest}'`], {
      stdio: "inherit",
    })
    if (ps.status !== 0) throw new Error("failed to unpack archive")
  } else {
    const tar = spawnSync("tar", ["-xzf", archive, "-C", dest], { stdio: "inherit" })
    if (tar.status !== 0) throw new Error("failed to unpack archive")
  }
}

async function main() {
  const triple = target()
  const asset = archiveName(triple)
  const tmp = join(tmpdir(), `codeus-npm-${Date.now()}`)
  const archive = join(tmp, asset)
  const checksums = join(tmp, "SHA256SUMS")
  const unpacked = join(tmp, "unpacked")
  const vendor = join(__dirname, "vendor")
  const exe = process.platform === "win32" ? "codeus.exe" : "codeus"

  mkdirSync(tmp, { recursive: true })
  try {
    await download(`${base}/${asset}`, archive)
    await download(`${base}/SHA256SUMS`, checksums)
    verify(archive, checksums, asset)
    unpack(archive, unpacked)

    const find = spawnSync(
      process.platform === "win32" ? "powershell" : "find",
      process.platform === "win32"
        ? ["-NoProfile", "-Command", `Get-ChildItem -Recurse -File '${unpacked}' -Filter 'codeus*.exe' | Select-Object -First 1 -ExpandProperty FullName`]
        : [unpacked, "-type", "f", "-name", "codeus*", "-perm", "-111"],
      { encoding: "utf8" },
    )
    const candidate = find.stdout.trim().split(/\r?\n/)[0]
    if (!candidate) throw new Error("archive did not contain a Codeus executable")

    mkdirSync(vendor, { recursive: true })
    copyFileSync(candidate, join(vendor, exe))
    chmodSync(join(vendor, exe), 0o755)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(`Failed to install Codeus CLI: ${error.message}`)
  console.error(`You can install manually with: curl -fsSL https://install.quantumclub.sh/codeus.sh | sh`)
  process.exit(1)
})
