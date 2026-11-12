// No DOM dependencies: execute the real renderer against text-only DOM nodes.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Node {
  constructor(tag) { this.tag = tag; this.children = []; this.textContent = ''; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  set innerHTML(_) { throw new Error('Unsafe HTML rendering'); }
  get text() { return this.textContent + this.children.map(c => c.text).join('\n'); }
  all() { return [this, ...this.children.flatMap(c => c.all())]; }
}
const values = { ready: 2, in_progress: 3, blocked: 4, review: 5, backlog: 6, done: 7 };
function snapshot() {
  return {
    started_at: 100, uptime_seconds: 123,
    coordinator: { state: 'reading_boards', latest_event: '<script>evil()</script>', started_at: 110, elapsed_seconds: 2 },
    overall: { counts: values, observed_projects: 1, total_projects: 2, stale: true },
    projects: [{ id: '<img onerror=evil()>', enabled: true, mode: 'master',
      story: { id: 'story', title: '<script>evil()</script>', url: 'javascript:evil()' },
      worker: { state: 'pending_update', latest_event: 'Completed; awaiting AppFlowy recording', started_at: 101, finished_at: 110, elapsed_seconds: 9 },
      polling: { last_poll_at: 120, last_change_at: 115, dirty: true, last_error: 'Board poll failed; last fingerprint retained' },
      observation: { counts: values, stale: true, read_started_at: 101, received_at: 110, reason: 'update_failed' }
    }, { id: 'disabled', enabled: false, mode: 'pr', story: null,
      worker: { state: 'disabled', latest_event: 'Not started', started_at: null },
      observation: { counts: null, stale: false, reason: null }
    }]
  };
}
function harness(fetch) {
  const nodes = Object.fromEntries(['connection', 'daemon', 'overall', 'projects'].map(id => [id, new Node('div')]));
  const timers = [];
  const context = vm.createContext({ document: { createElement: tag => new Node(tag), getElementById: id => nodes[id] },
    URL, Date, AbortSignal, fetch, setTimeout: (fn, ms) => timers.push({ fn, ms }) });
  vm.runInContext(fs.readFileSync('src/status.js', 'utf8'), context);
  return { nodes, timers, context };
}
const flush = () => new Promise(resolve => setImmediate(resolve));
test('renders unknown/stale counts, pending results and malicious titles as text', async () => {
  const { nodes, timers } = harness(async () => ({ ok: true, json: async () => snapshot() }));
  await flush();
  assert.match(nodes.connection.text, /Connected/);
  assert.match(nodes.overall.text, /Partial subtotal/);
  assert.match(nodes.projects.text, /Result waiting for AppFlowy/);
  assert.match(nodes.projects.text, /Stale.*last successful/);
  assert.match(nodes.projects.text, /Unknown.*no successful/);
  assert.match(nodes.projects.text, /<script>evil\(\)<\/script>/);
  assert.match(nodes.projects.text, /AppFlowy result update failed/);
  assert.match(nodes.projects.text, /Rust poll:/);
  assert.match(nodes.projects.text, /Coordination pending/);
  assert.match(nodes.projects.text, /Board poll failed; last fingerprint retained/);
  assert.equal(nodes.projects.all().filter(n => ['script', 'img'].includes(n.tag)).length, 0);
  assert.equal(nodes.projects.all().filter(n => n.href).length, 0);
  assert.equal(timers[0].ms, 3000);
});
test('refresh failure preserves data, marks disconnected, and recovers', async () => {
  let fail = false;
  const { nodes, timers } = harness(async (path, options) => {
    assert.equal(path, 'api/status'); assert.equal(options.cache, 'no-store');
    if (fail) throw new Error('offline');
    return { ok: true, json: async () => snapshot() };
  });
  await flush(); const before = nodes.projects.text;
  fail = true; await timers.shift().fn();
  assert.match(nodes.connection.text, /Disconnected.*stale/);
  assert.equal(nodes.projects.text, before);
  fail = false; await timers.shift().fn();
  assert.match(nodes.connection.text, /Connected/);
  assert.equal(nodes.connection.className, 'notice');
});
test('initial HTTP error stays unknown and links reject unsafe protocols/credentials', async () => {
  const { nodes, context } = harness(async () => ({ ok: false }));
  await flush();
  assert.match(nodes.connection.text, /No status received yet/);
  for (const value of ['javascript:alert(1)', 'data:text/html,evil', 'https://secret@host']) {
    context.value = value;
    assert.equal(vm.runInContext("link('story', value).tag", context), 'span');
  }
  context.value = 'https://appflowy.example/story';
  assert.equal(vm.runInContext("link('story', value).href", context), context.value);
});
