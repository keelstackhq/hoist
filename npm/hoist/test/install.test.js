"use strict";

// The npm packages as users get them: pack temp copies of the real package dirs with fixture
// release assets (as `hoist release publish` copies them), install the tarballs offline into a
// temp project and run `node_modules/.bin/hoist`. Nothing reaches a registry.
//
// HOIST_E2E_BIN=<path to a hoist binary> uses that binary as the host platform's asset and
// also checks `hoist --version`; otherwise the asset is a shell script.

const { test } = require("node:test");
const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const NPM_DIR = path.join(__dirname, "..", "..");
const LAUNCHER_DIR = path.join(NPM_DIR, "hoist");
const launcher = require(path.join(LAUNCHER_DIR, "bin", "hoist.js"));
const LAUNCHER_PKG = require(path.join(LAUNCHER_DIR, "package.json"));
const HOST = `${process.platform}-${process.arch}`;
const POSIX = process.platform !== "win32";
const NPM = POSIX ? "npm" : "npm.cmd";

const FIXTURE = '#!/bin/sh\necho "args:$*"\necho "fixture stderr" >&2\nexit 7\n';

/** npm with no registry access and no user config. */
function npm(args, cwd) {
  const r = spawnSync(NPM, args, {
    cwd,
    encoding: "utf8",
    shell: !POSIX,
    env: {
      ...process.env,
      npm_config_registry: "http://127.0.0.1:9/",
      npm_config_userconfig: path.join(cwd, ".npmrc-none"),
      npm_config_audit: "false",
      npm_config_fund: "false",
      npm_config_update_notifier: "false",
    },
  });
  assert.equal(r.status, 0, `npm ${args.join(" ")} failed:\n${r.stdout}\n${r.stderr}`);
  return r.stdout;
}

function tmp(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

/** Copy a package dir (without node_modules or a copied bin) into `to`. */
function copyPackage(from, to) {
  fs.cpSync(from, to, {
    recursive: true,
    filter: (src) => !src.includes(`${path.sep}node_modules`),
  });
}

/** Pack `dir` into `dest`; returns {file, files: [{path, mode}]}. */
function pack(dir, dest) {
  const out = JSON.parse(npm(["pack", "--json", "--pack-destination", dest], dir));
  return { file: path.join(dest, out[0].filename), files: out[0].files };
}

/** `platform-arch` -> its package dir under npm/. */
function platformDir(key) {
  return path.join(NPM_DIR, `hoist-${key}`);
}

/** Pack the platform package for `key` with `asset` as its binary (mode 0755). */
function packPlatform(key, asset, dest) {
  const dir = path.join(tmp("hoist-plat-"), `hoist-${key}`);
  copyPackage(platformDir(key), dir);
  const bin = path.join(dir, "bin", key.startsWith("win32-") ? "hoist.exe" : "hoist");
  fs.mkdirSync(path.dirname(bin), { recursive: true });
  fs.copyFileSync(asset, bin);
  fs.chmodSync(bin, 0o755);
  return pack(dir, dest);
}

function fixtureAsset() {
  if (process.env.HOIST_E2E_BIN) return path.resolve(process.env.HOIST_E2E_BIN);
  const file = path.join(tmp("hoist-asset-"), "hoist");
  fs.writeFileSync(file, FIXTURE);
  return file;
}

function packLauncher(dest) {
  const dir = path.join(tmp("hoist-launcher-"), "hoist");
  copyPackage(LAUNCHER_DIR, dir);
  return pack(dir, dest);
}

/** A temp project with `tarballs` installed offline; returns its dir. */
function install(tarballs) {
  const proj = tmp("hoist-proj-");
  fs.writeFileSync(path.join(proj, "package.json"), '{"name":"proj","version":"1.0.0"}\n');
  npm(["install", "--offline", "--no-save", ...tarballs], proj);
  return proj;
}

function runInstalled(proj, args) {
  const bin = path.join(proj, "node_modules", ".bin", POSIX ? "hoist" : "hoist.cmd");
  return spawnSync(bin, args, { encoding: "utf8", shell: !POSIX });
}

const hostSupported = Object.hasOwn(launcher.PLATFORMS, HOST);

test("each platform package packs its README, package.json and the binary", () => {
  const dest = tmp("hoist-tgz-");
  const asset = path.join(tmp("hoist-asset-"), "hoist");
  fs.writeFileSync(asset, FIXTURE);
  for (const [key, name] of Object.entries(launcher.PLATFORMS)) {
    const pkg = JSON.parse(fs.readFileSync(path.join(platformDir(key), "package.json"), "utf8"));
    const [os_, cpu] = key.split("-");
    assert.equal(pkg.name, name);
    assert.equal(pkg.version, LAUNCHER_PKG.version, `${name} version`);
    assert.deepEqual(pkg.os, [os_], `${name} os`);
    assert.deepEqual(pkg.cpu, [cpu], `${name} cpu`);
    const exe = os_ === "win32" ? "bin/hoist.exe" : "bin/hoist";
    const { files } = packPlatform(key, asset, dest);
    assert.deepEqual(
      files.map((f) => f.path).sort(),
      ["README.md", exe, "package.json"].sort(),
      `${name} tarball`,
    );
    if (POSIX && os_ !== "win32") {
      const mode = files.find((f) => f.path === exe).mode;
      assert.equal(mode & 0o111, 0o111, `${name} binary is executable in the tarball`);
    }
  }
});

test("the launcher tarball holds only the launcher", () => {
  const { files } = packLauncher(tmp("hoist-tgz-"));
  assert.deepEqual(
    files.map((f) => f.path).sort(),
    ["README.md", "bin/hoist.js", "package.json"].sort(),
  );
  assert.deepEqual(LAUNCHER_PKG.bin, { hoist: "bin/hoist.js" });
  for (const [dep, version] of Object.entries(LAUNCHER_PKG.optionalDependencies)) {
    assert.equal(version, LAUNCHER_PKG.version, `${dep} pinned to the launcher version`);
  }
});

test("the installed launcher runs the host platform's binary", (t) => {
  if (!POSIX || !hostSupported) {
    t.skip(`needs a POSIX host among ${Object.keys(launcher.PLATFORMS).join(", ")}`);
    return;
  }
  const dest = tmp("hoist-tgz-");
  const plat = packPlatform(HOST, fixtureAsset(), dest);
  const proj = install([plat.file, packLauncher(dest).file]);
  if (process.env.HOIST_E2E_BIN) {
    const r = runInstalled(proj, ["--version"]);
    assert.equal(r.status, 0, r.stderr);
    assert.match(r.stdout, new RegExp(`\\b${LAUNCHER_PKG.version.replaceAll(".", "\\.")}\\b`));
    return;
  }
  const r = runInstalled(proj, ["plan", "--json"]);
  assert.equal(r.status, 7, r.stderr);
  assert.equal(r.stdout, "args:plan --json\n");
  assert.equal(r.stderr, "fixture stderr\n");
});

test("the installed launcher without its platform package exits 1 naming it", (t) => {
  if (!POSIX || !hostSupported) {
    t.skip(`needs a POSIX host among ${Object.keys(launcher.PLATFORMS).join(", ")}`);
    return;
  }
  const proj = install([packLauncher(tmp("hoist-tgz-")).file]);
  const r = runInstalled(proj, ["check"]);
  assert.equal(r.status, 1);
  assert.ok(r.stderr.includes(launcher.PLATFORMS[HOST]), r.stderr);
});
