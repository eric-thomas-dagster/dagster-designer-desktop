#!/usr/bin/env node
// Cross-platform entry point for fetching uv's release binaries into
// vendor/uv/{uv,uvx} (see fetch-uv.sh's own header comment for why: so the
// app doesn't need uv, or transitively Python, pre-installed on the machine
// it runs on). package.json's app:build script calls this instead of
// fetch-uv.sh directly so the one script works under bash, cmd.exe and
// PowerShell alike -- npm scripts on Windows run through cmd.exe, which
// can't execute a bash script at all.
//
// macOS: shells out to the existing fetch-uv.sh unchanged (still needs
// bash + lipo to merge both Apple Silicon and Intel builds into one
// universal2 binary -- that's a real macOS-only tool, not worth
// reimplementing here).
// Windows: fetches the single x86_64-pc-windows-msvc build directly (no
// universal binary concept there) and unzips it with this script's own
// logic.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, copyFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const vendorDir = join(scriptDir, '..', 'vendor', 'uv');
const version = process.env.UV_VERSION || 'latest';
const force = process.env.FORCE === '1';

function alreadyFetched(names) {
  return !force && names.every((name) => existsSync(join(vendorDir, name)));
}

function releaseUrl(asset) {
  return version === 'latest'
    ? `https://github.com/astral-sh/uv/releases/latest/download/${asset}`
    : `https://github.com/astral-sh/uv/releases/download/${version}/${asset}`;
}

async function download(url, destPath) {
  console.log(`Fetching ${url}...`);
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`Failed to download ${url}: ${res.status} ${res.statusText}`);
  }
  const buf = Buffer.from(await res.arrayBuffer());
  const { writeFileSync } = await import('node:fs');
  writeFileSync(destPath, buf);
}

async function fetchWindows() {
  if (alreadyFetched(['uv', 'uvx'])) {
    console.log('vendor/uv/{uv,uvx} already present, skipping (set FORCE=1 to refetch)');
    return;
  }
  const workDir = mkdtempSync(join(tmpdir(), 'fetch-uv-'));
  try {
    const zipPath = join(workDir, 'uv.zip');
    await download(releaseUrl('uv-x86_64-pc-windows-msvc.zip'), zipPath);
    // Windows 10 1803+ / Server 2019+ ship a real bsdtar as tar.exe, which
    // (unlike GNU tar) also handles .zip -- same binary macOS's own
    // default `tar` is built on, so this call is identical on both.
    execFileSync('tar', ['-xf', zipPath, '-C', workDir]);
    mkdirSync(vendorDir, { recursive: true });
    // uv's Windows release zip extracts flat (uv.exe, uvx.exe, uv.pdb, ...)
    // at the archive root, unlike the Unix tarballs' uv-<target>/ subfolder.
    copyFileSync(join(workDir, 'uv.exe'), join(vendorDir, 'uv'));
    copyFileSync(join(workDir, 'uvx.exe'), join(vendorDir, 'uvx'));
    console.log(`Done: ${join(vendorDir, 'uv')}, ${join(vendorDir, 'uvx')}`);
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }
}

function fetchMac() {
  if (alreadyFetched(['uv', 'uvx'])) {
    console.log('vendor/uv/{uv,uvx} already present, skipping (set FORCE=1 to refetch)');
    return;
  }
  execFileSync('bash', [join(scriptDir, 'fetch-uv.sh')], {
    stdio: 'inherit',
    env: process.env,
  });
}

if (process.platform === 'win32') {
  await fetchWindows();
} else if (process.platform === 'darwin') {
  fetchMac();
} else {
  throw new Error(`fetch-uv.mjs: unsupported platform ${process.platform} (only darwin and win32 are set up so far)`);
}
