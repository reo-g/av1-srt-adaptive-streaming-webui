// Fails when implementation terms leak into the UI source or the built bundle.
// The UI and the API speak product vocabulary only (docs/terms.md).
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const ROOTS = ['src', 'dist'];
const RULES = [
  /\bsrt\b/i,
  /\blibsrt\b/i,
  /gstreamer/i,
  /\bgst[-_]/i,
  /\biperf/i,
  /\bSRTO_/,
  /\bSRT_REJ/,
  /["'](caller|listener|rendezvous)["']/i,
  /\bpassphrase_len|pbkeylen\b/i,
];

const files = [];
const walk = (d) => {
  let entries = [];
  try {
    entries = readdirSync(d);
  } catch {
    return;
  }
  for (const e of entries) {
    const p = join(d, e);
    if (statSync(p).isDirectory()) walk(p);
    else if (/\.(tsx?|jsx?|css|html|json|mjs)$/.test(p)) files.push(p);
  }
};
ROOTS.forEach(walk);

let bad = 0;
for (const f of files) {
  const lines = readFileSync(f, 'utf8').split('\n');
  lines.forEach((line, i) => {
    for (const r of RULES) {
      const m = line.match(r);
      if (m) {
        bad++;
        console.error(`${f}:${i + 1}: "${m[0]}"`);
      }
    }
  });
}
if (bad) {
  console.error(`\n${bad} implementation term(s) found. Use the words in docs/terms.md instead.`);
  process.exit(1);
}
console.log(`check-terms: ${files.length} files clean`);
