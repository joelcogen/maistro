// No DOM dependencies: execute the real renderer against text-only DOM nodes.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
class Node {
  constructor(tag, document) {
    this.tag = tag; this.document = document; this.children = []; this.textContent = ''; this.dataset = {};
  }
  setAttribute(name, value) { this[name] = value; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) {
    if (this.all().includes(this.document.activeElement)) this.document.activeElement = null;
    this.children = children;
  }
  querySelectorAll(selector) {
    assert.equal(selector, 'a[data-status-link]');
    return this.all().filter(node => node.tag === 'a' && node.dataset.statusLink);
  }
  focus() { this.document.activeElement = this; }
  set innerHTML(_) { throw new Error('Unsafe HTML rendering'); }
  get text() { return this.textContent + this.children.map(c => c.text).filter(Boolean).join('\n'); }
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
  const document = { activeElement: null, createElement: tag => new Node(tag, document), getElementById: id => nodes[id] };
  const ids = ['connection', 'connection-state', 'connection-detail', 'uptime', 'daemon', 'overall', 'projects', 'agents-section', 'projects-section'];
  const nodes = Object.fromEntries(ids.map(id => [id, new Node('div', document)]));
  nodes.connection.className = 'status status-yellow';
  nodes['connection-state'].textContent = 'Connecting…';
  nodes['agents-section'].hidden = nodes['projects-section'].hidden = true;
  const timers = [];
  const context = vm.createContext({ document,
    URL, Date, AbortSignal, fetch, setTimeout: (fn, ms) => timers.push({ fn, ms }) });
  vm.runInContext(fs.readFileSync('src/status.js', 'utf8'), context);
  return { nodes, timers, context, document };
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const withClass = (node, className) => node.all().filter(n => n.className?.split(' ').includes(className));

test('renders unknown/stale counts, pending results and malicious titles as text', async () => {
  const { nodes, timers } = harness(async () => ({ ok: true, json: async () => snapshot() }));
  await flush();
  assert.equal(nodes['connection-state'].text, 'Connected');
  assert.equal(nodes['agents-section'].hidden, false);
  assert.equal(nodes['projects-section'].hidden, false);
  assert.match(nodes.overall.text, /Partial subtotal/);
  assert.match(nodes.projects.text, /Result waiting for board/);
  assert.equal(withClass(nodes.projects, 'card-footer')[0].text,
    `Stale counts · Last successful read ${new Date(101000).toLocaleString()}`);
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
    assert.ok(options.signal instanceof AbortSignal);
    if (fail) throw new Error('offline');
    return { ok: true, json: async () => snapshot() };
  });
  await flush(); const before = nodes.projects.text;
  fail = true; await timers.shift().fn();
  assert.equal(nodes['connection-state'].text, 'Disconnected');
  assert.equal(nodes.connection.className, 'status status-red');
  assert.match(nodes['connection-detail'].text, /stale.*Last received.*Retrying/);
  assert.equal(nodes.projects.text, before);
  fail = false; await timers.shift().fn();
  assert.equal(nodes['connection-state'].text, 'Connected');
  assert.equal(nodes.connection.className, 'status status-green');
  assert.match(nodes['connection-detail'].text, /^Updated /);
});
test('initial HTTP error stays unknown and links reject unsafe protocols/credentials', async () => {
  const { nodes, context } = harness(async () => ({ ok: false }));
  assert.equal(nodes['connection-state'].text, 'Connecting…');
  await flush();
  assert.equal(nodes['connection-state'].text, 'Disconnected');
  assert.match(nodes['connection-detail'].text, /No status received yet.*Retrying/);
  assert.equal(nodes['agents-section'].hidden, true);
  assert.equal(nodes['projects-section'].hidden, true);
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

test('refresh restores keyboard focus for the same story on the same project', async () => {
  const data = snapshot();
  const project = data.projects[0];
  project.story.url = 'https://example.test/story/123';
  data.projects.push({ ...project, id: 'other' });
  const { nodes, timers, document } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  nodes.projects.children[0].querySelectorAll('a[data-status-link]')[0].focus();

  data.projects.reverse();
  await timers.shift().fn();
  assert.equal(document.activeElement, nodes.projects.children[2].querySelectorAll('a[data-status-link]')[0]);

  project.story = { ...project.story, id: 'replacement' };
  await timers.shift().fn();
  assert.equal(document.activeElement, null);
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
  const names = row => withClass(row, 'fw-semibold')[0].text;
  assert.deepEqual(rows.map(names),
    ['Coordinator', '<img onerror=evil()>', 'disabled', 'active', 'stopping', 'idle']);
  assert.deepEqual(rows.map(row => withClass(row, 'status')[0].text),
    ['Idle', 'Result waiting for board', 'Disabled', 'Worker active', 'Worker stopping', 'Idle']);
  assert.deepEqual(rows.filter(row => withClass(row, 'status-green').length).map(names), ['active', 'stopping']);
  assert.deepEqual(rows.filter(row => withClass(row, 'status-yellow').length).map(names), ['<img onerror=evil()>']);
  assert.doesNotMatch(nodes.daemon.text, /Started|Finished|Duration|Verbose event|<script>/);
});

test('agents show configured effort for each role, including disabled workers, without inferring defaults', async () => {
  const data = snapshot();
  data.coordinator.configured_effort = 'low';
  data.projects[0].worker.configured_effort = 'high';
  data.projects[1].worker.configured_effort = 'high';
  const { nodes, context } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  const efforts = () => withClass(nodes.daemon, 'worker-config').map(n => n.text);
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
  assert.equal(withClass(nodes.daemon, 'status-green')[0].text, 'Coordinating');
  assert.match(nodes.daemon.text, /Last coordination pass: 6 minutes ago/);
});

test('overview and project counts share compact labels and full observations need no note', async () => {
  const data = snapshot();
  data.overall = { counts: values, observed_projects: 2, total_projects: 2, stale: false };
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.equal(nodes.overall.children.length, 1);
  const grid = nodes.overall.children[0];
  const labels = grid => grid.all().filter(n => n.tag === 'span' && n.textContent).map(n => n.text);
  const counts = grid => withClass(grid, 'tabular-nums').map(n => n.text);
  assert.deepEqual(labels(grid), ['Backlog', 'Ready', 'In Progress', 'Review', 'Done', 'Blocked']);
  assert.deepEqual(counts(grid), ['6', '2', '3', '5', '7', '4']);
  assert.deepEqual(withClass(grid, 'status-dot').map(n => n.className),
    ['secondary', 'blue', 'yellow', 'indigo', 'green', 'pink'].map(color => `status-dot status-${color}`));
  const projectGrid = withClass(nodes.projects.children[0], 'counts')[0];
  assert.equal(grid.text, projectGrid.text);
  const unknownGrid = withClass(nodes.projects.children[1], 'counts')[0];
  assert.deepEqual(counts(unknownGrid), Array(6).fill('Unknown'));
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
