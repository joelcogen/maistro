// No DOM dependencies: execute the real renderer against text-only DOM nodes.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Node {
  constructor(tag) { this.tag = tag; this.children = []; this.textContent = ''; }
  setAttribute(name, value) { this[name] = value; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  set innerHTML(_) { throw new Error('Unsafe HTML rendering'); }
  get text() { return this.textContent + this.children.map(c => c.text).join('\n'); }
  all() { return [this, ...this.children.flatMap(c => c.all())]; }
}
const values = { ready: 2, in_progress: 3, blocked: 4, review: 5, backlog: 6, done: 7 };
function snapshot() {
  return {
    started_at: 100, generated_at: 470, uptime_seconds: 123,
    coordinator: { state: 'reading_boards', latest_event: '<script>evil()</script>', started_at: 110, elapsed_seconds: 2 },
    overall: { counts: values, observed_projects: 1, total_projects: 2, stale: true },
    projects: [{ id: '<img onerror=evil()>', enabled: true, mode: 'master',
      story: { id: 'story', title: '<script>evil()</script>', url: 'javascript:evil()' },
      worker: { state: 'pending_update', latest_event: 'Completed; awaiting board recording', started_at: 101, finished_at: 110, elapsed_seconds: 9 },
      polling: { last_poll_at: 120, last_change_at: 115, dirty: true, last_error: 'Board poll failed; last fingerprint retained' },
      observation: { counts: values, stale: true, read_started_at: 101, received_at: 110, reason: 'update_failed' }
    }, { id: 'disabled', enabled: false, mode: 'pr', story: null,
      worker: { state: 'disabled', latest_event: 'Not started', started_at: null },
      observation: { counts: null, stale: false, reason: null }
    }]
  };
}
function harness(fetch) {
  const nodes = Object.fromEntries(['connection', 'uptime', 'daemon', 'overall', 'projects'].map(id => [id, new Node('div')]));
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
  assert.match(nodes.projects.text, /Result waiting for board/);
  assert.match(nodes.projects.text, /Stale counts/);
  assert.match(nodes.projects.text, /Unknown.*no complete/);
  assert.match(nodes.projects.text, /<script>evil\(\)<\/script>/);
  assert.match(nodes.projects.text, /Board result update failed/);
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

test('board and story links remain available without polling details', async () => {
  const data = snapshot();
  data.projects = [data.projects[0]];
  data.projects[0].board_url = 'https://app.shortcut.com/team/settings/workflows/42';
  data.projects[0].story.url = 'https://app.shortcut.com/team/story/123';
  data.projects[0].polling = { method: 'coordinator', last_poll_at: null, dirty: false };
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.doesNotMatch(nodes.projects.text, /Periodic coordinator reads/);
  assert.doesNotMatch(nodes.projects.text, /Rust poll:|AppFlowy/);
  assert.deepEqual(nodes.projects.all().filter(n => n.href).map(n => n.href),
    [data.projects[0].board_url, data.projects[0].story.url]);
});

test('agents list starts with the coordinator followed by each project and its status', async () => {
  const data = snapshot();
  data.coordinator.state = 'idle';
  data.projects.push(...['active', 'stopping', 'idle'].map(state => ({
    ...data.projects[0], id: state, worker: { state, latest_event: 'Verbose event' }
  })));
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.match(nodes.uptime.text, /Uptime 0h 2m 3s/);
  assert.match(nodes.daemon.text, /Last coordination pass: 6 minutes ago/);
  const rows = nodes.daemon.all().filter(n => n.tag === 'li');
  assert.deepEqual(rows.map(n => n.children[0].children[0].text),
    ['Coordinator', '<img onerror=evil()>', 'disabled', 'active', 'stopping', 'idle']);
  assert.deepEqual(rows.map(n => n.children[1]['aria-label']),
    ['Idle', 'Result waiting for board', 'Disabled', 'Worker active', 'Worker stopping', 'Idle']);
  assert.deepEqual(rows.filter(n => n.children[1].className.includes('running')).map(n => n.children[0].children[0].text), ['active', 'stopping']);
  assert.doesNotMatch(nodes.daemon.text, /Started|Finished|Duration|Verbose event|<script>|Result waiting/);
});

test('agents show configured effort for each role, including disabled workers, without inferring defaults', async () => {
  const data = snapshot();
  data.coordinator.configured_effort = 'low';
  data.projects[0].worker.configured_effort = 'high';
  data.projects[1].worker.configured_effort = 'high';
  const { nodes, context } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  const efforts = () => nodes.daemon.all().filter(n => n.className === 'worker-config').map(n => n.text);
  assert.deepEqual(efforts(), ['Default model · low', 'Default model · high', 'Default model · high']);
  data.coordinator.configured_effort = null;
  data.projects[0].worker.configured_effort = null;
  context.data = data;
  vm.runInContext('render(data)', context);
  assert.deepEqual(efforts(), ['Default model · default', 'Default model · default', 'Default model · high']);
  assert.doesNotMatch(nodes.daemon.text, /effective|runtime effort/i);
});

test('coordination age handles first run, active passes and time boundaries', async () => {
  const { nodes, context } = harness(async () => ({ ok: true, json: async () => ({
    ...snapshot(), coordinator: { state: 'idle', started_at: null }
  }) }));
  await flush();
  assert.match(nodes.daemon.text, /Last coordination pass: Not yet/);
  for (const [seconds, expected] of [[-1, 'just now'], [0, 'just now'], [59, 'just now'], [60, '1 minute ago'], [3600, '1 hour ago'], [7200, '2 hours ago'], [86400, '1 day ago']]) {
    context.seconds = seconds;
    assert.equal(vm.runInContext('ago(100, 100 + seconds)', context), expected);
  }
  context.data = snapshot();
  vm.runInContext('render(data)', context);
  assert.equal(nodes.daemon.all().find(n => n.className === 'agent-indicator running')['aria-label'], 'Coordinating');
  assert.match(nodes.daemon.text, /Last coordination pass: 6 minutes ago/);
});

test('overview and project counts share compact labels and full observations need no note', async () => {
  const data = snapshot();
  data.overall = { counts: values, observed_projects: 2, total_projects: 2, stale: false };
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.equal(nodes.overall.children.length, 1);
  const grid = nodes.overall.children[0];
  assert.equal(grid.className, 'counts');
  assert.deepEqual(grid.children.map(n => n.children[1].text), ['Backlog', 'Ready', 'In Progress', 'Review', 'Done', 'Blocked']);
  assert.deepEqual(grid.children.map(n => n.children[0].text), ['6', '2', '3', '5', '7', '4']);
  const projectGrid = nodes.projects.children[0].all().find(n => n.className === 'counts');
  assert.equal(grid.text, projectGrid.text);
  const unknownGrid = nodes.projects.children[1].all().find(n => n.className === 'counts');
  assert.equal(unknownGrid.children.filter(n => n.children[0].text === 'Unknown').length, 6);
  assert.doesNotMatch(nodes.projects.text, /Git mode|Current process|Board observation|Story story|No current story|Started|Rust poll|Last change/);
  assert.ok(nodes.projects.text.includes(`Finished ${new Date(110000).toLocaleString()}`));
  assert.match(nodes.projects.text, /0h 0m 9s/);
  assert.ok(nodes.projects.text.includes(new Date(101000).toLocaleString()));
});

 test('model names remain text and appear beside effort in compact agent cards', async () => {
  const data = snapshot();
  data.coordinator.configured_model = 'gpt-6-luna';
  data.coordinator.configured_effort = 'medium';
  data.projects[0].worker.configured_model = '<script>model</script>';
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.match(nodes.daemon.text, /gpt-6-luna · medium/);
  assert.match(nodes.daemon.text, /<script>model<\/script> · default/);
  assert.equal(nodes.daemon.all().filter(n => n.tag === 'script').length, 0);
});
