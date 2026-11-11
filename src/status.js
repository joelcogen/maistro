'use strict';
const labels = { ready: 'Ready · queued', in_progress: 'In Progress · active/interrupted', blocked: 'Blocked', review: 'Review', backlog: 'Backlog', done: 'Done' };
const reasons = {
  no_eligible_work: 'No eligible work found', dependencies: 'Waiting for dependencies', occupied: 'Project already occupied',
  read_failed: 'Board read failed', ambiguous_board: 'Board could not be resolved uniquely', incomplete_read: 'Board read incomplete',
  claim_failed: 'Could not confirm a story claim', requirements_blocked: 'Story requirements blocked', update_failed: 'AppFlowy result update failed', work_selected: 'Work selected'
};
const states = { active: 'Worker active', pending_update: 'Result waiting for AppFlowy', idle: 'Idle', disabled: 'Disabled', stopping: 'Worker stopping', reading_boards: 'Coordinating' };
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
function activity(value) {
  const box = element('div');
  box.append(element('p', states[value.state] || value.state), element('p', value.latest_event));
  if (value.last_error) box.append(element('p', value.last_error));
  if (value.started_at != null) box.append(element('p', `Started ${date(value.started_at)} · Duration ${duration(value.elapsed_seconds)}${value.finished_at == null ? ' · running' : ` · Finished ${date(value.finished_at)}`}`, 'muted'));
  return box;
}
function render(snapshot) {
  const daemon = document.getElementById('daemon');
  daemon.replaceChildren(element('p', `Uptime ${duration(snapshot.uptime_seconds)} · Started ${date(snapshot.started_at)}`), activity(snapshot.coordinator));
  const overall = snapshot.overall;
  document.getElementById('overall').replaceChildren(
    element('p', `${overall.observed_projects}/${overall.total_projects} configured projects observed${overall.observed_projects < overall.total_projects ? ' · Partial subtotal; unobserved projects are unknown' : ''}${overall.stale ? ' · Stale observations included' : ''}`),
    counts(overall.counts), element('p', 'Observation times differ by project; see each read window below.', 'muted'));
  const projects = document.getElementById('projects'); projects.replaceChildren();
  for (const project of snapshot.projects) {
    const card = element('article'); const observation = project.observation;
    card.append(element('h2', project.id), element('p', `${project.enabled ? 'Enabled' : 'Disabled'} · Git mode: ${project.mode}`, 'muted'));
    if (project.board_url) card.append(link('Open AppFlowy board', project.board_url));
    card.append(element('h3', 'Current process'), activity(project.worker));
    if (project.last_outcome) card.append(element('p', `Latest worker result: ${project.last_outcome}`, 'muted'));
    if (project.story) {
      card.append(element('p', project.story.title), element('p', `Story ${project.story.id}`, 'muted'));
      if (project.story.url) card.append(link('Open AppFlowy story', project.story.url));
    } else card.append(element('p', 'No current story', 'muted'));
    card.append(element('h3', 'Board observation'));
    card.append(element('p', observation.counts == null ? 'Unknown · no successful complete board read' : `${observation.stale ? 'Stale · last successful observation retained' : 'Latest complete observation'} · Read during ${date(observation.read_started_at)} – ${date(observation.received_at)}`));
    if (observation.stale && observation.counts == null) card.append(element('p', 'Latest pass did not provide a complete board read'));
    if (observation.reason) card.append(element('p', reasons[observation.reason] || observation.reason));
    if (!project.enabled) card.append(element('p', 'Disabled projects are not read or dispatched'));
    card.append(counts(observation.counts)); projects.append(card);
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
