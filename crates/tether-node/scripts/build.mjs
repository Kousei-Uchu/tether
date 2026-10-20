// Builds the Rust addon and copies it next to index.js as tether.node.
// Usage: node scripts/build.mjs [--release]
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crateDir = join(here, '..');
const repoRoot = join(crateDir, '..', '..');
const release = process.argv.includes('--release');

const args = ['build', '-p', 'tether-node'];
if (release) args.push('--release');
const run = spawnSync('cargo', args, { cwd: repoRoot, stdio: 'inherit' });
if (run.status !== 0) {
  console.error('cargo build failed. Is Rust installed? See the README.');
  process.exit(run.status ?? 1);
}

const libName = {
  win32: 'tether_node.dll',
  darwin: 'libtether_node.dylib',
}[process.platform] ?? 'libtether_node.so';

const built = join(repoRoot, 'target', release ? 'release' : 'debug', libName);
if (!existsSync(built)) {
  console.error(`expected ${built} but it was not produced`);
  process.exit(1);
}
copyFileSync(built, join(crateDir, 'tether.node'));
console.log(`copied ${libName} to crates/tether-node/tether.node`);
