'use strict';
const labels = { ready: 'Ready', in_progress: 'In Progress', blocked: 'Blocked', review: 'Review', backlog: 'Backlog', done: 'Done' };
const reasons = {
  no_eligible_work: 'No eligible work found', dependencies: 'Waiting for dependencies', occupied: 'Project already occupied',
  read_failed: 'Board read failed', ambiguous_board: 'Board could not be resolved uniquely', incomplete_read: 'Board read incomplete',
  claim_failed: 'Could not confirm a story claim', requirements_blocked: 'Story requirements blocked', update_failed: 'Board result update failed', work_selected: 'Work selected'
};
const states = { active: 'Worker active', pending_update: 'Result waiting for board', idle: 'Idle', disabled: 'Disabled', stopping: 'Worker stopping', reading_boards: 'Coordinating' };
function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = String(text);
  if (className) node.className = className;
  return node;
}
function date(value) { return value == null ? 'Unknown' : new Date(value * 1000).toLocaleString(); }
function duration(seconds) {
  return `${Math.floor(seconds / 3600)}h ${Math.floor(seconds % 3600 / 60)}m ${seconds % 60}s`;
}
function link(label, value) {
  try {
    const url = new URL(value);
    if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password) throw new Error('Unsafe link');
    const node = element('a', label); node.href = url.href; node.rel = 'noreferrer'; return node;
  } catch { return element('span', label); }
}
function counts(values) {
  const grid = element('div', undefined, 'counts');
  for (const [key, label] of Object.entries(labels)) {
    const cell = element('div', undefined, values == null ? 'count unknown' : 'count');
    cell.append(element('strong', values == null ? 'Unknown' : values[key]), element('span', label)); grid.append(cell);
  }
  return grid;
}
function ago(value, now) {
  if (value == null) return 'Not yet';
  const seconds = Math.max(0, now - value);
  if (seconds < 60) return 'just now';
  for (const [size, unit] of [[86400, 'day'], [3600, 'hour'], [60, 'minute']]) {
    if (seconds >= size) {
      const amount = Math.floor(seconds / size);
      return `${amount} ${unit}${amount === 1 ? '' : 's'} ago`;
    }
  }
}
function workerRunning(worker) { return ['active', 'stopping'].includes(worker.state); }
function render(snapshot) {
  const daemon = document.getElementById('daemon');
  const workers = element('ul', undefined, 'worker-list');
  const coordinating = snapshot.coordinator.state === 'reading_boards';
  const coordinator = element('li', undefined, 'row');
  coordinator.append(element('span', 'Coordinator', 'worker-name'),
    element('span', states[snapshot.coordinator.state] || snapshot.coordinator.state, coordinating ? 'worker-state running' : 'worker-state'));
  workers.append(coordinator);
  for (const project of snapshot.projects) {
    const running = workerRunning(project.worker);
    const row = element('li', undefined, 'row');
    const name = element('span', project.id, 'worker-name');
    name.title = project.id;
    row.append(name, element('span', !project.enabled ? 'Disabled' : running ? 'Running' : 'Idle', running ? 'worker-state running' : 'worker-state'));
    workers.append(row);
  }
  daemon.replaceChildren(workers,
    element('p', `Uptime ${duration(snapshot.uptime_seconds)}`, 'muted'),
    element('p', `Last coordination pass: ${ago(snapshot.coordinator.started_at, snapshot.generated_at)}`, 'muted'));
  const overall = snapshot.overall;
  const overview = document.getElementById('overall');
  overview.replaceChildren(counts(overall.counts));
  const coverage = [];
  if (overall.observed_projects < overall.total_projects) coverage.push(`Partial subtotal · ${overall.observed_projects}/${overall.total_projects} projects observed`);
  if (overall.stale) coverage.push('Stale counts');
  if (coverage.length) overview.append(element('p', coverage.join(' · '), 'warning'));
  const projects = document.getElementById('projects'); projects.replaceChildren();
  for (const project of snapshot.projects) {
    const card = element('article'); const observation = project.observation;
    const heading = element('div', undefined, 'row card-heading');
    heading.append(element('h2', project.id));
    if (project.board_url) heading.append(link('Open board', project.board_url));
    card.append(heading);
    const worker = project.worker;
    card.append(element('p', `${states[worker.state] || worker.state}${worker.started_at == null ? '' : ` · ${duration(worker.elapsed_seconds)}`}`));
    if (worker.started_at != null) card.append(element('p', worker.finished_at == null ? `Started ${date(worker.started_at)}` : `Finished ${date(worker.finished_at)}`, 'muted'));
    if (worker.last_error) card.append(element('p', worker.last_error, 'warning'));
    if (project.story) {
      const story = element('p', undefined, 'story');
      story.append(project.story.url ? link(project.story.title, project.story.url) : element('span', project.story.title));
      card.append(story);
    }
    if (project.polling?.last_error) card.append(element('p', project.polling.last_error, 'warning'));
    if (observation.reason && !['no_eligible_work', 'occupied', 'work_selected'].includes(observation.reason)) {
      card.append(element('p', reasons[observation.reason] || observation.reason, 'warning'));
    }
    card.append(counts(observation.counts));
    // Use the pass start: receipt may be later than the board read or status writes.
    card.append(element('p', observation.counts == null ? 'Unknown · no complete board read' : `${observation.stale ? 'Stale counts' : 'Observed'} · ${date(observation.read_started_at)}`, observation.stale ? 'warning' : 'muted'));
    projects.append(card);
  }
}
let lastReceived = null;
async function refresh() {
  const connection = document.getElementById('connection');
  try {
    const response = await fetch('api/status', { cache: 'no-store', signal: AbortSignal.timeout(5000) });
    if (!response.ok) throw new Error('Status unavailable');
    const snapshot = await response.json(); render(snapshot);
    lastReceived = Date.now();
    connection.textContent = `Connected · Updated ${new Date(lastReceived).toLocaleTimeString()}`;
    connection.className = 'notice';
  } catch {
    connection.textContent = `Disconnected · displayed information is stale. ${lastReceived == null ? 'No status received yet.' : `Last received ${new Date(lastReceived).toLocaleString()}.`} Retrying…`;
    connection.className = 'notice error';
  } finally { setTimeout(refresh, 3000); }
}
refresh();
