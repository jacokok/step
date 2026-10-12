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
  let dialogCount = 0;
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const promise = pending.get(message.id); pending.delete(message.id);
      if (message.error) promise.reject(Error(message.error.message)); else promise.resolve(message.result);
    }
    if (message.method === 'Page.javascriptDialogOpening') {
      dialogCount++;
      call('Page.handleJavaScriptDialog', { accept: true }, message.sessionId);
    }
  });
  const { targetId } = await call('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await call('Target.attachToTarget', { targetId, flatten: true });
  await call('Page.enable', {}, sessionId);
  await call('Emulation.setDeviceMetricsOverride', { width: 1100, height: 820, deviceScaleFactor: 1, mobile: false }, sessionId);
  async function evaluate(expression) {
    const result = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw Error(result.exceptionDetails.exception.description);
    return result.result.value;
  }
  await call('Page.navigate', { url }, sessionId);
  await waitFor(() => evaluate("typeof track !== 'undefined' && track && !busy && audio.readyState >= 1"), 'initial editor ready');
  assert.equal(await evaluate('audio.duration'), 4);
  assert.equal(await evaluate('audio.hidden && !audio.controls'), true);
  assert.equal(await evaluate('document.querySelectorAll("input[type=number]").length'), 0);
  assert.equal(await evaluate('Array.from(document.querySelectorAll(".icon-button")).every(button => button.getAttribute("aria-label") && button.title && button.querySelector("svg"))'), true);
  await evaluate("$('togglePlay').click()");
  await waitFor(() => evaluate('!audio.paused && audio.currentTime > 0'), 'icon toolbar playback');
  assert.equal(await evaluate("$('playIcon').getAttribute('href')"), '#icon-pause');
  await evaluate("$('togglePlay').click()");
  async function pressSpace(autoRepeat = false) {
    await call('Input.dispatchKeyEvent', { type: 'keyDown', key: ' ', code: 'Space', windowsVirtualKeyCode: 32, autoRepeat }, sessionId);
    await call('Input.dispatchKeyEvent', { type: 'keyUp', key: ' ', code: 'Space', windowsVirtualKeyCode: 32 }, sessionId);
  }
  for (const id of ['tracks', 'markIn', 'remove', 'optionsButton', 'togglePlay']) {
    await evaluate(`selection(0,1); audio.pause(); $('${id}').focus()`);
    await pressSpace();
    await waitFor(() => evaluate('!audio.paused'), `Space plays with ${id} focused`);
    await pressSpace(true);
    assert.equal(await evaluate('audio.paused'), false, 'held Space must not repeatedly toggle playback');
    await pressSpace();
    assert.equal(await evaluate('audio.paused'), true, `Space pauses with ${id} focused`);
    assert.equal(await evaluate('track.duration'), 4, 'Space on trash must never delete');
    assert.equal(await evaluate("$('options').matches(':popover-open')"), false, 'Space on options must not activate its button');
  }
  await evaluate("$('optionsButton').click(); $('snap').focus()");
  const snapBeforeSpace = await evaluate("$('snap').checked");
  await pressSpace();
  await waitFor(() => evaluate('!audio.paused'), 'Space plays with snapping checkbox focused');
  assert.equal(await evaluate("$('snap').checked"), snapBeforeSpace, 'Space must not toggle the focused checkbox');
  await pressSpace();
  assert.equal(await evaluate('audio.paused'), true);
  await waitFor(() => evaluate("$('optionsButton').getAttribute('aria-expanded') === 'true'"), 'options menu open');
  assert.equal(await evaluate("$('options').matches(':popover-open')"), true);
  await evaluate("$('snap').checked = true; $('options').hidePopover(); seek(0); $('markIn').click(); seek(1); $('markOut').click()");
  assert.deepEqual(await evaluate('bounds()'), [0, 1]);
  const span = await evaluate('viewEnd-viewStart');
  await evaluate("$('zoomIn').click()");
  assert.ok(await evaluate('viewEnd-viewStart') < span);
  await evaluate("$('overview').click()");
  assert.equal(await evaluate('viewEnd-viewStart'), 4);
  const rect = await evaluate('(() => { const r=canvas.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height}; })()');
  await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x + rect.width / 4, y: rect.y + rect.height / 2 }, sessionId);
  assert.equal(await evaluate('getComputedStyle(canvas).cursor'), 'ew-resize', 'hovering a selection handle shows a resize cursor');
  await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }, sessionId);
  assert.equal(await evaluate('getComputedStyle(canvas).cursor'), 'crosshair', 'waveform keeps its normal cursor away from handles');
  async function dragEnd(from, to) {
    const y = rect.y + rect.height / 2;
    await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: rect.x + from / 4 * rect.width, y, button: 'left', clickCount: 1 }, sessionId);
    await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: rect.x + to / 4 * rect.width, y, button: 'left', buttons: 1 }, sessionId);
    assert.equal(await evaluate('getComputedStyle(canvas).cursor'), 'ew-resize', 'resize cursor remains while dragging');
    await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: rect.x + to / 4 * rect.width, y, button: 'left', clickCount: 1 }, sessionId);
  }
  await dragEnd(1, 1.5);
  assert.deepEqual(await evaluate('bounds()'), [0, 1.5]);
  await dragEnd(1.5, 1);
  assert.deepEqual(await evaluate('bounds()'), [0, 1]);
  if (process.env.EDITOR_SCREENSHOT) {
    const { data } = await call('Page.captureScreenshot', { format: 'png' }, sessionId);
    writeFileSync(process.env.EDITOR_SCREENSHOT, Buffer.from(data, 'base64'));
  }
  await call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true }, sessionId);
  assert.equal(await evaluate('document.documentElement.scrollWidth <= innerWidth'), true, 'mobile toolbar must not overflow');
  await evaluate("$('optionsButton').click()");
  await waitFor(() => evaluate("$('optionsButton').getAttribute('aria-expanded') === 'true'"), 'mobile options menu');
  assert.equal(await evaluate("(() => {const r=$('options').getBoundingClientRect();return r.left>=0 && r.right<=innerWidth && r.top>=0;})()"), true);
  await evaluate("$('options').hidePopover()");
  await call('Emulation.setDeviceMetricsOverride', { width: 1100, height: 820, deviceScaleFactor: 1, mobile: false }, sessionId);
  // Hold the real POST at the browser fetch boundary so the busy UI and
  // duplicate-click guard can be asserted deterministically, then let FFmpeg run.
  await evaluate(`window.__realFetch=window.fetch;window.__removeCalls=0;
    window.fetch=async (path,options)=>{if(path==='/api/remove'){window.__removeCalls++;await new Promise(resolve=>window.__releaseDelete=resolve);}return window.__realFetch(path,options);};`);
  await evaluate("$('remove').click()");
  await waitFor(() => evaluate('window.__removeCalls === 1'), 'delete request started');
  assert.equal(await evaluate("!$('deleteProgress').hidden"), true, 'delete shows a visible processing indicator');
  assert.equal(await evaluate("$('remove').getAttribute('aria-busy')"), 'true');
  assert.equal(await evaluate("$('remove').disabled && busy"), true);
  await evaluate("$('remove').click(); document.dispatchEvent(new KeyboardEvent('keydown',{key:'Delete',bubbles:true}));");
  assert.equal(await evaluate('window.__removeCalls'), 1, 'repeated activation cannot submit duplicate cuts');
  await evaluate('window.fetch=window.__realFetch;window.__releaseDelete()');
  await waitFor(() => evaluate('track && !busy && audio.readyState >= 1'), 'editor ready after cut');
  assert.equal(await evaluate("$('deleteProgress').hidden"), true);
  assert.equal(await evaluate("$('remove').getAttribute('aria-busy')"), 'false');
  assert.equal(dialogCount, 0, 'deletion does not open a confirmation dialog');
  assert.equal(await evaluate('audio.duration'), 3, 'playback must use the edited file before controls are enabled');
  assert.deepEqual(await evaluate('track.beats'), [0, .5, 1, 1.5, 2, 2.5]);
  assert.equal(await evaluate("$('snap').disabled"), false);
  assert.equal(await evaluate("$('snap').checked"), true, 'snap preference survives the reload');
  assert.equal(await evaluate('snapped(.6)'), .5);
  await evaluate("canvas.focus(); document.dispatchEvent(new KeyboardEvent('keydown', {key:' ', bubbles:true}));");
  await waitFor(() => evaluate('!audio.paused && audio.currentTime > 0'), 'keyboard playback after cut');
  // The processing indicator must cover the reload too, not just encoding.
  await evaluate(`window.__reloadHeld=false;window.fetch=async (path,options)=>{
    if(path.startsWith('/api/track/')){window.__reloadHeld=true;await new Promise(resolve=>window.__releaseReload=resolve);}
    return window.__realFetch(path,options);
  };audio.pause();selection(2,3);void remove();`);
  await waitFor(() => evaluate('window.__reloadHeld'), 'waveform refresh after cut');
  assert.equal(await evaluate("!$('deleteProgress').hidden && busy && deleting"), true, 'loading remains visible during refresh');
  assert.equal(await evaluate("$('deleteProgressText').textContent"), 'Updating waveform…');
  await evaluate('window.fetch=window.__realFetch;window.__releaseReload()');
  await waitFor(() => evaluate('track && !busy && audio.readyState >= 1'), 'editor ready after second cut');
  assert.equal(await evaluate('audio.duration'), 2);
  assert.deepEqual(await evaluate('track.beats'), [0, .5, 1, 1.5]);
  // A rejected cut must also restore the player, rather than leave it detached.
  await evaluate("track.hash = 'stale'; selection(0,.5); remove()");
  assert.match(await evaluate("$('status').textContent"), /Track changed/);
  assert.equal(await evaluate('busy'), false);
  assert.equal(await evaluate("$('deleteProgress').hidden"), true, 'failure also clears the processing indicator');
  assert.equal(await evaluate('audio.duration'), 2);
  await evaluate("canvas.focus(); document.dispatchEvent(new KeyboardEvent('keydown', {key:' ', bubbles:true}));");
  await waitFor(() => evaluate('!audio.paused && audio.currentTime > 0'), 'playback after rejected cut');
  // Changing tracks leaves the selector focused; Delete must still cut the
  // waveform selection rather than silently being swallowed by that control.
  await evaluate("audio.pause(); selection(.5,1); $('tracks').focus()");
  await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46 }, sessionId);
  await call('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46 }, sessionId);
  await waitFor(() => evaluate('track?.duration === 1.5 && !busy'), 'Delete with track selector focused');
  assert.equal(dialogCount, 0);
  await evaluate("selection(0,.25); $('optionsButton').click()");
  await waitFor(() => evaluate("$('options').matches(':popover-open')"), 'snapping control available');
  await evaluate("$('snap').focus()");
  await call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46 }, sessionId);
  await call('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Delete', code: 'Delete', windowsVirtualKeyCode: 46 }, sessionId);
  await waitFor(() => evaluate('track?.duration === 1.25 && !busy'), 'Delete with snapping checkbox focused');
  assert.equal(await evaluate("$('options').matches(':popover-open')"), false);
  // Empty/whole-track selections cannot be cut, and their disabled state is
  // explicit rather than a clickable button that seems to do nothing.
  assert.equal(await evaluate("$('remove').disabled"), true);
  await evaluate('selection(0,track.duration)');
  assert.equal(await evaluate("$('remove').disabled"), true);
  await evaluate('selection(.25,.5)');
  assert.equal(await evaluate("$('remove').disabled"), false);
});
