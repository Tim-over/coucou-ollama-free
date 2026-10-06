// Builds the Claude Code relay (coucou-hook) as a Windows .exe and puts it where
// the Tauri bundler expects it: target/release/coucou-hook.exe.
//
// On Windows, a plain `cargo build` does that. When building from Linux/macOS
// (how Coucou's Windows installer is produced here), the hook has to be
// cross-compiled with cargo-xwin and the result copied into target/release/.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const run = (cmd, args) =>
  execFileSync(cmd, args, { cwd: root, stdio: "inherit", shell: process.platform === "win32" });

const dest = resolve(root, "target/release/coucou-hook.exe");

if (process.platform === "win32") {
  run("cargo", ["build", "--release", "-p", "coucou-hook"]);
} else {
  const target = "x86_64-pc-windows-msvc";
  run("cargo", ["xwin", "build", "--release", "-p", "coucou-hook", "--target", target]);
  const built = resolve(root, `target/${target}/release/coucou-hook.exe`);
  if (!existsSync(built)) {
    console.error(`build-hook: expected ${built} but it is missing`);
    process.exit(1);
  }
  mkdirSync(dirname(dest), { recursive: true });
  copyFileSync(built, dest);
  console.log(`build-hook: ${built} -> ${dest}`);
}
