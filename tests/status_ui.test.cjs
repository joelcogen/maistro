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
    const [tag] = selector.split('[');
    return this.all().filter(node => (!tag || node.tag === tag) && node.dataset.statusLink);
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
  return { nodes, timers, document };
}
const flush = () => new Promise(resolve => setImmediate(resolve));

test('renders unknown/stale counts, pending results and malicious titles as text', async () => {
  const { nodes, timers } = harness(async () => ({ ok: true, json: async () => snapshot() }));
  await flush();
  assert.equal(nodes['connection-state'].text, 'Connected');
  assert.equal(nodes['agents-section'].hidden, false);
  assert.equal(nodes['projects-section'].hidden, false);
  assert.match(nodes.overall.text, /Partial subtotal/);
  assert.match(nodes.projects.text, /Result waiting for board/);
  assert.ok(nodes.projects.text.includes(
    `Stale counts · Last successful read ${new Date(101000).toLocaleString()}`));
  assert.match(nodes.projects.text, /Unknown.*no complete/);
  assert.match(nodes.projects.text, /<script>evil\(\)<\/script>/);
  assert.match(nodes.projects.text, /Board result update failed/);
  assert.match(nodes.projects.text, /Board poll failed; last fingerprint retained/);
  assert.equal(nodes.projects.all().filter(n => ['script', 'img'].includes(n.tag)).length, 0);
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
  assert.match(nodes['connection-detail'].text, /stale.*Last received.*Retrying/);
  assert.equal(nodes.projects.text, before);
  fail = false; await timers.shift().fn();
  assert.equal(nodes['connection-state'].text, 'Connected');
  assert.match(nodes['connection-detail'].text, /^Updated /);
});
test('initial HTTP error leaves status sections hidden', async () => {
  const { nodes } = harness(async () => ({ ok: false }));
  assert.equal(nodes['connection-state'].text, 'Connecting…');
  await flush();
  assert.equal(nodes['connection-state'].text, 'Disconnected');
  assert.match(nodes['connection-detail'].text, /No status received yet.*Retrying/);
  assert.equal(nodes['agents-section'].hidden, true);
  assert.equal(nodes['projects-section'].hidden, true);
});

test('links with unsafe protocols or credentials show only their label', async () => {
  const data = snapshot();
  data.projects = ['javascript:alert(1)', 'data:text/html,evil', 'https://secret@host'].map((url, index) => ({
    ...data.projects[0], id: `project-${index}`,
    story: { id: 'story', title: `Story ${index}`, url }, board_url: url
  }));
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  for (let index = 0; index < data.projects.length; index++) {
    assert.ok(nodes.projects.text.includes(`Story ${index}`));
  }
  assert.equal(nodes.projects.all().filter(node => node.href).length, 0);
});

test('board and story links remain available without polling details', async () => {
  const data = snapshot();
  data.projects = [data.projects[0]];
  data.projects[0].board_url = 'https://app.shortcut.com/team/settings/workflows/42';
  data.projects[0].story.url = 'https://app.shortcut.com/team/story/123';
  data.projects[0].polling = { method: 'coordinator', last_poll_at: null, dirty: false };
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
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
  const projectLink = () => nodes.projects.all()
    .find(node => node.tag === 'article' && node.text.includes(project.id))
    .all().find(node => node.href === project.story.url);
  projectLink().focus();

  data.projects.reverse();
  await timers.shift().fn();
  assert.equal(document.activeElement, projectLink());

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
  const rows = nodes.daemon.all().filter(node => node.tag === 'li');
  assert.equal(rows.length, 6);
  for (const [index, name, status] of [
    [0, 'Coordinator', 'Idle'],
    [1, '<img onerror=evil()>', 'Result waiting for board'],
    [2, 'disabled', 'Disabled'],
    [3, 'active', 'Worker active'],
    [4, 'stopping', 'Worker stopping'],
    [5, 'idle', 'Idle']
  ]) {
    assert.ok(rows[index].text.startsWith(name + '\n'));
    assert.ok(rows[index].text.includes('\n' + status));
  }
});

test('agents show configured models and effort as text with defaults for omitted settings', async () => {
  const data = snapshot();
  data.coordinator.configured_model = 'gpt-6-luna';
  data.coordinator.configured_effort = 'medium';
  data.projects[0].worker.configured_model = '<script>model</script>';
  data.projects[1].worker.configured_effort = 'high';
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.match(nodes.daemon.text, /Coordinator\ngpt-6-luna · medium/);
  assert.match(nodes.daemon.text, /<script>model<\/script> · default/);
  assert.match(nodes.daemon.text, /disabled\nDefault model · high/);
  assert.equal(nodes.daemon.all().filter(node => node.tag === 'script').length, 0);
});

test('coordination age handles first run, active passes and time boundaries', async () => {
  const data = snapshot();
  data.coordinator.started_at = null;
  const { nodes, timers } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  assert.match(nodes.daemon.text, /Last coordination pass: Not yet/);

  data.coordinator.started_at = 100;
  for (const [seconds, expected] of [
    [-1, 'just now'], [59, 'just now'], [60, '1 minute ago'],
    [3600, '1 hour ago'], [7200, '2 hours ago'], [86400, '1 day ago']
  ]) {
    data.generated_at = 100 + seconds;
    await timers.shift().fn();
    assert.ok(nodes.daemon.text.includes(`Last coordination pass: ${expected}`));
  }
  assert.match(nodes.daemon.text, /Coordinating/);
});

test('overview and project counts share compact labels and full observations need no note', async () => {
  const data = snapshot();
  data.overall = { counts: values, observed_projects: 2, total_projects: 2, stale: false };
  const { nodes } = harness(async () => ({ ok: true, json: async () => data }));
  await flush();
  const [project, unknown] = nodes.projects.all().filter(node => node.tag === 'article');
  for (const [label, count] of [
    ['Backlog', 6], ['Ready', 2], ['In Progress', 3], ['Review', 5], ['Done', 7], ['Blocked', 4]
  ]) {
    assert.ok(nodes.overall.text.includes(`${label}\n${count}`));
    assert.ok(project.text.includes(`${label}\n${count}`));
    assert.ok(unknown.text.includes(`${label}\nUnknown`));
  }
  assert.doesNotMatch(nodes.overall.text, /Partial subtotal|Stale counts/);
  assert.ok(nodes.projects.text.includes(`Finished ${new Date(110000).toLocaleString()}`));
  assert.match(nodes.projects.text, /0h 0m 9s/);
  assert.ok(nodes.projects.text.includes(new Date(101000).toLocaleString()));
});
