// Builds switchyard-core in release mode and places it where Tauri's
// `externalBin` expects it: src-tauri/binaries/switchyard-core-<target-triple>.
// Tauri then bundles it next to the app executable (Contents/MacOS), where the
// desktop launcher finds it.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const rustcInfo = execFileSync("rustc", ["-vV"], { cwd: root, encoding: "utf8" });
const triple = /^host: (\S+)$/m.exec(rustcInfo)?.[1];
if (!triple) throw new Error("cannot determine host target triple from `rustc -vV`");

execFileSync("cargo", ["build", "--release", "-p", "switchyard-core"], { cwd: root, stdio: "inherit" });

const outDir = join(root, "apps/desktop/src-tauri/binaries");
mkdirSync(outDir, { recursive: true });
const dest = join(outDir, `switchyard-core-${triple}`);
copyFileSync(join(root, "target/release/switchyard-core"), dest);
console.log(`sidecar ready: ${dest}`);
