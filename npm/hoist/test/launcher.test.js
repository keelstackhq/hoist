"use strict";

const { test } = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const LAUNCHER = path.join(__dirname, "..", "bin", "hoist.js");
const launcher = require(LAUNCHER);

test("maps each supported platform to its package", () => {
  assert.equal(launcher.packageFor("linux", "x64"), "@keelstackhq/hoist-linux-x64");
  assert.equal(launcher.packageFor("linux", "arm64"), "@keelstackhq/hoist-linux-arm64");
  assert.equal(launcher.packageFor("darwin", "x64"), "@keelstackhq/hoist-darwin-x64");
  assert.equal(launcher.packageFor("darwin", "arm64"), "@keelstackhq/hoist-darwin-arm64");
  assert.equal(launcher.packageFor("win32", "x64"), "@keelstackhq/hoist-win32-x64");
  assert.equal(launcher.packageFor("win32", "arm64"), undefined);
  assert.equal(launcher.packageFor("freebsd", "x64"), undefined);
});

test("platform packages match the launcher's optionalDependencies", () => {
  const pkg = require("../package.json");
  const names = Object.values(launcher.PLATFORMS).sort();
  assert.deepEqual(Object.keys(pkg.optionalDependencies).sort(), names);
  assert.deepEqual(Object.keys(pkg.bin), ["hoist"]);
});

test("the binary is bin/hoist, with .exe on Windows", () => {
  const resolve = (id) => {
    assert.match(id, /^@keelstackhq\/hoist-[a-z0-9]+-[a-z0-9]+\/package\.json$/);
    return path.join("/pkgs", id);
  };
  assert.equal(
    launcher.binaryPath("linux", "x64", resolve),
    path.join("/pkgs/@keelstackhq/hoist-linux-x64", "bin", "hoist"),
  );
  assert.equal(
    launcher.binaryPath("win32", "x64", resolve),
    path.join("/pkgs/@keelstackhq/hoist-win32-x64", "bin", "hoist.exe"),
  );
});

/** Run the launcher with process.platform/arch faked through a preload module. */
function runAs(platform, arch, args = []) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hoist-launcher-"));
  const preload = path.join(dir, "fake.js");
  fs.writeFileSync(
    preload,
    `Object.defineProperty(process, "platform", { value: ${JSON.stringify(platform)} });\n` +
      `Object.defineProperty(process, "arch", { value: ${JSON.stringify(arch)} });\n`,
  );
  return spawnSync(process.execPath, ["--require", preload, LAUNCHER, ...args], {
    encoding: "utf8",
  });
}

test("an unknown platform exits 1 and lists the supported ones", () => {
  const r = runAs("aix", "ppc64");
  assert.equal(r.status, 1);
  assert.match(r.stderr, /aix-ppc64/);
  for (const p of ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"]) {
    assert.ok(r.stderr.includes(p), `stderr lists ${p}: ${r.stderr}`);
  }
});

test("a missing platform package exits 1 and names it", () => {
  const r = runAs("linux", "arm64");
  assert.equal(r.status, 1);
  assert.match(r.stderr, /@keelstackhq\/hoist-linux-arm64/);
});

test("runs the binary with the arguments and passes its exit code through", (t) => {
  if (process.platform === "win32") {
    t.skip("uses a POSIX shell script as the binary");
    return;
  }
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hoist-bin-"));
  const pkg = path.join(dir, "node_modules", launcher.packageFor(process.platform, process.arch));
  fs.mkdirSync(path.join(pkg, "bin"), { recursive: true });
  fs.writeFileSync(path.join(pkg, "package.json"), "{}\n");
  const bin = path.join(pkg, "bin", "hoist");
  fs.writeFileSync(bin, '#!/bin/sh\necho "args:$*"\nexit 7\n');
  fs.chmodSync(bin, 0o755);
  const r = spawnSync(
    process.execPath,
    [
      "-e",
      `process.exitCode = require(${JSON.stringify(LAUNCHER)}).run(process.argv.slice(1), ` +
        `(id) => require.resolve(id, { paths: [${JSON.stringify(dir)}] }));`,
      "plan",
      "--json",
    ],
    { encoding: "utf8" },
  );
  assert.equal(r.status, 7, r.stderr);
  assert.equal(r.stdout, "args:plan --json\n");
});

test("a binary without the executable bit is made executable once and run", (t) => {
  if (process.platform === "win32") {
    t.skip("uses a POSIX shell script as the binary");
    return;
  }
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hoist-bin-"));
  const pkg = path.join(dir, "node_modules", launcher.packageFor(process.platform, process.arch));
  fs.mkdirSync(path.join(pkg, "bin"), { recursive: true });
  fs.writeFileSync(path.join(pkg, "package.json"), "{}\n");
  const bin = path.join(pkg, "bin", "hoist");
  fs.writeFileSync(bin, '#!/bin/sh\necho "ran:$*"\nexit 3\n');
  fs.chmodSync(bin, 0o644);
  const r = spawnSync(
    process.execPath,
    [
      "-e",
      `process.exitCode = require(${JSON.stringify(LAUNCHER)}).run(process.argv.slice(1), ` +
        `(id) => require.resolve(id, { paths: [${JSON.stringify(dir)}] }));`,
      "check",
    ],
    { encoding: "utf8" },
  );
  assert.equal(r.status, 3, r.stderr);
  assert.equal(r.stdout, "ran:check\n");
  assert.equal(fs.statSync(bin).mode & 0o777, 0o755);
});
