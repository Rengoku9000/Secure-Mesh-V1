/**
 * Builds a standalone SecureMesh binary and stages it outside `target/`.
 *
 * # Why this exists
 *
 * `src-tauri/target/debug/securemesh[.exe]` is written by two different build
 * paths that mean different things:
 *
 * | Command                                | Result                                    |
 * |----------------------------------------|-------------------------------------------|
 * | `cargo build` / `test` / `clippy` / `run` | **dev** binary: loads the UI from `devUrl` |
 * | `tauri build`                          | **production** binary: UI assets embedded |
 *
 * They share one path, so the last one to run wins. Staging a production build
 * and running `cargo test` afterwards silently replaces it with a dev binary,
 * and launching that without a dev server shows only ERR_CONNECTION_REFUSED.
 *
 * Copying the binary somewhere `cargo` never writes removes the collision
 * entirely, so a staged binary stays runnable no matter what is built later.
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const isWindows = process.platform === "win32";
const binaryName = isWindows ? "securemesh.exe" : "securemesh";

const builtBinary = join(projectRoot, "src-tauri", "target", "debug", binaryName);
const stageDir = join(projectRoot, "dist-app");
const stagedBinary = join(stageDir, binaryName);

function run(command, args) {
  console.log(`> ${command} ${args.join(" ")}`);
  execFileSync(command, args, {
    cwd: projectRoot,
    stdio: "inherit",
    shell: isWindows, // npm/npx resolve through .cmd shims on Windows
  });
}

/**
 * Fails unless the binary carries the frontend inside it.
 *
 * The whole point of staging is a binary that runs on its own, so this verifies
 * that rather than trusting the build flags.
 *
 * Note *which* property is checked. Both build modes embed `tauri.conf.json`,
 * so both contain the `devUrl` string — its presence proves nothing. What
 * separates them is the **frontend assets**: a production build embeds them, a
 * development build does not, because it expects to fetch them from the dev
 * server. Checking for the asset filenames referenced by `dist/index.html`
 * tests the property that actually decides whether the window renders.
 */
function assertStandalone(path) {
  const indexHtml = readFileSync(join(projectRoot, "dist", "index.html"), "utf8");
  const assets = [...indexHtml.matchAll(/(?:src|href)="\/(assets\/[^"]+)"/g)].map(
    (match) => match[1],
  );

  if (assets.length === 0) {
    throw new Error(
      "dist/index.html references no bundled assets — run `npm run build` first.",
    );
  }

  const contents = readFileSync(path).toString("latin1");
  const missing = assets.filter((asset) => !contents.includes(asset));

  if (missing.length > 0) {
    throw new Error(
      `the staged binary does not embed the frontend (missing ${missing.join(", ")}), ` +
        `so it is a development build and will show ERR_CONNECTION_REFUSED without a ` +
        `dev server. Expected a production build from \`tauri build\`.`,
    );
  }

  console.log(`verified: ${assets.length} frontend asset(s) embedded — the binary is standalone`);
}

// `--no-bundle` skips installer generation, which is not wanted for a local
// run. `--debug` keeps the build fast and the symbols useful; the frontend is
// embedded either way, which is the property that matters here.
run("npm", ["run", "tauri", "--", "build", "--debug", "--no-bundle"]);

// Verify before copying, so a bad build never leaves a broken staged binary
// behind for someone to run.
assertStandalone(builtBinary);

rmSync(stageDir, { recursive: true, force: true });
mkdirSync(stageDir, { recursive: true });
copyFileSync(builtBinary, stagedBinary);

console.log(`\nStaged: ${stagedBinary}`);
console.log("Run two nodes, each with its own data directory:\n");
if (isWindows) {
  console.log(`  $env:SECUREMESH_DATA_DIR="$env:TEMP\\smA"; Start-Process ${stagedBinary}`);
  console.log(`  $env:SECUREMESH_DATA_DIR="$env:TEMP\\smB"; Start-Process ${stagedBinary}`);
} else {
  console.log(`  SECUREMESH_DATA_DIR=/tmp/smA ${stagedBinary} &`);
  console.log(`  SECUREMESH_DATA_DIR=/tmp/smB ${stagedBinary} &`);
}
