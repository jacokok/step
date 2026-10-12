// Run: cargo build && node --test tests/editor-browser.mjs
// Requires Chromium (CHROME_BIN may override its path) and FFmpeg.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';

async function waitFor(check, label, timeout = 10000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw Error('Timed out: ' + label);
}
async function outputMatch(child, stream, pattern) {
  let output = '';
  child[stream].on('data', chunk => { output += chunk; });
  return waitFor(() => {
    if (child.exitCode !== null) throw Error(`Process exited: ${output}`);
    return output.match(pattern)?.[1];
  }, 'process startup');
}

test('cutting leaves playback and further edits usable without refreshing', { timeout: 40000 }, async t => {
  const dir = mkdtempSync(join(tmpdir(), 'step-editor-browser-'));
  const children = [];
  t.after(async () => {
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill();
        await once(child, 'exit');
      }
    }
    rmSync(dir, { recursive: true, force: true });
  });
  mkdirSync(join(dir, 'tracks'));
  execFileSync('ffmpeg', ['-nostdin', '-v', 'error', '-f', 'lavfi', '-i', 'sine=frequency=440:duration=4', join(dir, 'tracks', '00001-test.wav')]);
  async function startEditor() {
    const editor = spawn('target/debug/step', ['edit', '--work-dir', dir]); children.push(editor);
    return { editor, url: await outputMatch(editor, 'stdout', /Open (http:\/\/\S+)/) };
  }
  // Prime real PCM metadata, then supply deterministic detector markers rather
  // than requiring a model download in this browser regression test.
  const prime = await startEditor();
  const detail = await fetch(prime.url + '/api/track/1'); assert.equal(detail.status, 200); await detail.json();
  prime.editor.kill(); await once(prime.editor, 'exit');
  const manifestPath = join(dir, 'manifest.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  manifest.tracks[0].analysis = {
    cache_key: 'fixture', detector: 'fixture', bpm: 120, confidence: 1,
    confidence_kind: 'fixture', interval_regularity: 1,
    beats: [.5, 1, 1.5, 2, 2.5, 3, 3.5].map(seconds => ({ seconds, sample: seconds * 48000, score: 1 })),
    downbeats: [1, 3], warnings: [],
  };
  writeFileSync(manifestPath, JSON.stringify(manifest));
  const { url } = await startEditor();
  const chrome = spawn(process.env.CHROME_BIN || '/opt/google/chrome/chrome', ['--headless', '--no-sandbox', '--disable-gpu', '--autoplay-policy=no-user-gesture-required', '--remote-debugging-port=0', '--user-data-dir=' + join(dir, 'chrome'), 'about:blank']); children.push(chrome);
  const debuggerUrl = await outputMatch(chrome, 'stderr', /DevTools listening on (ws:\/\/\S+)/);
  const socket = new WebSocket(debuggerUrl);
  await once(socket, 'open'); t.after(() => socket.close());
  let id = 0;
  const pending = new Map();
  function call(method, params = {}, sessionId) {
    return new Promise((resolve, reject) => {
      const next = ++id; pending.set(next, { resolve, reject });
      socket.send(JSON.stringify({ id: next, method, params, sessionId }));
    });
  }
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const promise = pending.get(message.id); pending.delete(message.id);
      if (message.error) promise.reject(Error(message.error.message)); else promise.resolve(message.result);
    }
    if (message.method === 'Page.javascriptDialogOpening') call('Page.handleJavaScriptDialog', { accept: true }, message.sessionId);
  });
  const { targetId } = await call('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await call('Target.attachToTarget', { targetId, flatten: true });
  await call('Page.enable', {}, sessionId);
  async function evaluate(expression) {
    const result = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw Error(result.exceptionDetails.exception.description);
    return result.result.value;
  }
  await call('Page.navigate', { url }, sessionId);
  await waitFor(() => evaluate("typeof track !== 'undefined' && track && !busy && audio.readyState >= 1"), 'initial editor ready');
  assert.equal(await evaluate('audio.duration'), 4);
  await evaluate("$('snap').checked = true");
  // The real UI path: set a selection, confirm a cut, then immediately play.
  await evaluate('selection(0, 1); remove()');
  await waitFor(() => evaluate('track && !busy && audio.readyState >= 1'), 'editor ready after cut');
  assert.equal(await evaluate('audio.duration'), 3, 'playback must use the edited file before controls are enabled');
  assert.deepEqual(await evaluate('track.beats'), [0, .5, 1, 1.5, 2, 2.5]);
  assert.equal(await evaluate("$('snap').disabled"), false);
  assert.equal(await evaluate("$('snap').checked"), true, 'snap preference survives the reload');
  assert.equal(await evaluate('snapped(.6)'), .5);
  await evaluate("canvas.focus(); document.dispatchEvent(new KeyboardEvent('keydown', {key:' ', bubbles:true}));");
  await waitFor(() => evaluate('!audio.paused && audio.currentTime > 0'), 'keyboard playback after cut');
  await evaluate('audio.pause(); selection(2,3); remove()');
  await waitFor(() => evaluate('track && !busy && audio.readyState >= 1'), 'editor ready after second cut');
  assert.equal(await evaluate('audio.duration'), 2);
  assert.deepEqual(await evaluate('track.beats'), [0, .5, 1, 1.5]);
  // A rejected cut must also restore the player, rather than leave it detached.
  await evaluate("track.hash = 'stale'; selection(0,.5); remove()");
  assert.match(await evaluate("$('status').textContent"), /Track changed/);
  assert.equal(await evaluate('busy'), false);
  assert.equal(await evaluate('audio.duration'), 2);
  await evaluate("canvas.focus(); document.dispatchEvent(new KeyboardEvent('keydown', {key:' ', bubbles:true}));");
  await waitFor(() => evaluate('!audio.paused && audio.currentTime > 0'), 'playback after rejected cut');
});
