// Builds the Rust bridge in release mode and copies it into apps/desktop/native/, which is
// what gets packaged. main.js loads from there when it exists.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const desktop = join(here, '..');
const bridge = join(desktop, '..', '..', 'crates', 'tether-node');

const run = spawnSync(process.execPath, [join(bridge, 'scripts', 'build.mjs'), '--release'], {
  stdio: 'inherit',
});
if (run.status !== 0) process.exit(run.status ?? 1);

const out = join(desktop, 'native');
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
for (const f of ['index.js', 'index.d.ts', 'tether.node']) copyFileSync(join(bridge, f), join(out, f));
console.log('staged the bridge in apps/desktop/native');
