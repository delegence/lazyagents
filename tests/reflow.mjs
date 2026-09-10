// npm install --prefix target/terminal-tests --no-save ghostty-web@0.4.0 @xterm/headless@6.0.0
// node tests/reflow.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { Ghostty } from '../target/terminal-tests/node_modules/ghostty-web/dist/ghostty-web.js';
import xterm from '../target/terminal-tests/node_modules/@xterm/headless/lib-headless/xterm-headless.js';

const root = new URL('../', import.meta.url);
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'lazyagents-reflow-'));
try {
  const trace = path.join(temporary, 'trace.json');
  const result = spawnSync('cargo', ['test', '--locked', '--bin', 'lazyagents', 'export_reflow_trace', '--', '--ignored'], {
    cwd: root, env: { ...process.env, LAZYAGENTS_REFLOW_TRACE: trace }, stdio: 'inherit',
  });
  assert.equal(result.status, 0, 'trace generation failed');
  const wasm = await WebAssembly.instantiate(fs.readFileSync(new URL('../target/terminal-tests/node_modules/ghostty-web/ghostty-vt.wasm', import.meta.url)), { env: { log() {} } });
  const ghostty = new Ghostty(wasm.instance).createTerminal(80, 30);
  const xt = new xterm.Terminal({ cols: 80, rows: 30, allowProposedApi: true, scrollback: 10000 });
  const engines = [
    {
      name: 'Ghostty',
      write: text => ghostty.write(text), resize: size => ghostty.resize(...size),
      text: () => Array.from({ length: ghostty.getScrollbackLength() + ghostty.rows }, (_, i) => (i < ghostty.getScrollbackLength() ? ghostty.getScrollbackLine(i) : ghostty.getLine(i - ghostty.getScrollbackLength()))
        .filter(c => c.width !== 0).map(c => String.fromCodePoint(c.codepoint || 32)).join('').trimEnd()).join('\n').trimEnd(),
      alternate: () => ghostty.getMode(1049),
    },
    {
      name: 'xterm.js',
      write: text => new Promise(resolve => xt.write(text, resolve)), resize: size => xt.resize(...size),
      text: () => Array.from({ length: xt.buffer.active.length }, (_, i) => xt.buffer.active.getLine(i)?.translateToString(true) || '').join('\n').trimEnd(),
      alternate: () => xt.buffer.active.type === 'alternate',
    },
  ];
  for (const [step, op] of JSON.parse(fs.readFileSync(trace)).entries()) {
    for (const engine of engines) {
      if (op.write) await engine.write(op.write);
      if (op.resize) engine.resize(op.resize);
      if (!op.check) continue;
      const text = engine.text();
      const compact = text.replace(/\s/g, '');
      const context = `${engine.name}, step ${step}:\n${text}`;
      assert.equal(engine.alternate(), false, context);
      assert(compact.includes('PREVIOUSshelloutputwithanintactlongsuffixSENTINEL'), context);
      assert(compact.includes('AssistantresponsethatmustreturnafterresizingRESPONSE_END'), context);
      assert.equal(compact.split('coach').length - 1, 1, context);
      assert(!text.includes('/status'), context);
      if (op.check === 'draft') assert(compact.includes('draftwithalongsuffixDRAFT_END'), context);
      if (op.check === 'finished') assert(compact.includes('statusresultSTATUS_END'), context);

    }
  }
  xt.dispose();
  console.log('Ghostty and xterm.js: inline resize preserves earlier output, prints one header, and keeps commands hidden.');
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
