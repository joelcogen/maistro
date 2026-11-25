'use strict';
const storyStates = {
  backlog: ['Backlog', 'secondary'],
  ready: ['Ready', 'blue'],
  in_progress: ['In Progress', 'yellow'],
  review: ['Review', 'indigo'],
  done: ['Done', 'green'],
  blocked: ['Blocked', 'pink']
};
const reasons = {
  no_eligible_work: 'No eligible work found', dependencies: 'Waiting for dependencies', occupied: 'Project already occupied',
  read_failed: 'Board read failed', ambiguous_board: 'Board could not be resolved uniquely', incomplete_read: 'Board read incomplete',
  claim_failed: 'Could not confirm a story claim', requirements_blocked: 'Story requirements blocked', update_failed: 'Board result update failed', work_selected: 'Work selected'
};
const states = { active: 'Worker active', dreaming: 'Dreaming', pending_update: 'Result waiting for board', idle: 'Idle', disabled: 'Disabled', stopping: 'Worker stopping', reading_boards: 'Coordinating' };
const activityColors = { active: 'green', dreaming: 'purple', stopping: 'green', reading_boards: 'green', pending_update: 'yellow' };
const failedBoardActions = new Set(['read_failed', 'claim_failed', 'update_failed']);
const pendingDreams = new Set();
let lastSnapshot;
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
function link(label, value, className, focusKey) {
  try {
    const url = new URL(value);
    if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password) throw new Error('Unsafe link');
    const node = element('a', label, className);
    node.href = url.href;
    node.rel = 'noreferrer';
    if (focusKey) node.dataset.statusFocus = JSON.stringify(focusKey);
    return node;
  } catch { return element('span', label); }
}
function dot(color) {
  const node = element('span', undefined, color ? `status-dot status-${color}` : 'status-dot');
  node.setAttribute('aria-hidden', 'true');
  return node;
}
function counts(values, compact = false) {
  const spacing = compact ? 'g-2' : 'g-2 g-sm-3';
  const grid = element('div', undefined, `counts row row-deck row-cols-3 row-cols-lg-6 ${spacing}`);
  for (const [key, [label, color]] of Object.entries(storyStates)) {
    const column = element('div', undefined, 'col');
    const card = element('div', undefined, `card card-sm${compact ? ' bg-surface-secondary' : ''}`);
    const body = element('div', undefined, compact ? 'card-body p-2' : 'card-body');
    const name = element('div', undefined, 'd-flex align-items-center gap-1 small text-secondary mb-1');
    name.append(dot(color), element('span', label));
    const valueStyle = values == null ? 'small text-secondary' : compact ? 'h2' : 'h1';
    body.append(name, element('p', values == null ? 'Unknown' : values[key], `tabular-nums mb-0 ${valueStyle}`));
    card.append(body);
    column.append(card);
    grid.append(column);
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
function activityStatus(activity, enabled) {
  const state = enabled ? activity.state : 'disabled';
  const badge = element('span', undefined, `status status-${activityColors[state] || 'secondary'}`);
  badge.append(dot(), element('span', states[state] || state));
  return badge;
}
function agentRow(name, activity, enabled = true) {
  const row = element('li', undefined, 'col');
  const card = element('div', undefined, 'card card-sm');
  const body = element('div', undefined, 'card-body d-flex align-items-center justify-content-between gap-2');
  const info = element('div');
  const title = element('div', name, 'fw-semibold');
  const model = activity.configured_model ?? 'Default model';
  const effort = activity.configured_effort ?? 'default';
  const config = element('div', `${model} · ${effort}`, 'worker-config small text-secondary');
  config.title = `Configured model: ${model} · Effort: ${activity.configured_effort ?? 'inherited/default'}`;
  info.append(title, config);
  body.append(info, activityStatus(activity, enabled));
  card.append(body);
  row.append(card);
  return row;
}
async function requestDream(id) {
  if (pendingDreams.has(id)) return;
  pendingDreams.add(id);
  const feedback = document.getElementById('dream-feedback');
  feedback.className = 'small mb-2 text-secondary';
  feedback.textContent = `Checking dream eligibility for ${id}…`;
  render(lastSnapshot);
  try {
    const response = await fetch('api/dream', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      referrerPolicy: 'same-origin',
      body: JSON.stringify({ project_id: id })
    });
    if (!response.ok) throw new Error(await response.text());
    feedback.className = 'small mb-2 text-secondary';
    feedback.textContent = `Dreaming started for ${id}.`;
  } catch (error) {
    feedback.className = 'small mb-2 text-danger-emphasis';
    feedback.textContent = `Could not start dreaming for ${id}. ${error.message}`;
  } finally {
    pendingDreams.delete(id);
    render(lastSnapshot);
  }
}
function projectCard(project) {
  const card = element('article', undefined, 'card');
  const heading = element('header', undefined, 'card-header d-flex flex-wrap justify-content-between gap-2 py-2 px-3');
  heading.append(element('h3', project.id, 'card-title mb-0'));
  const actions = element('div', undefined, 'd-flex flex-wrap gap-2');
  if (project.enabled) {
    const checking = pendingDreams.has(project.id);
    const dream = element('button', checking ? 'Checking…' : 'Dream now', 'btn btn-sm btn-outline-primary');
    dream.type = 'button';
    dream.disabled = checking || project.worker.state !== 'idle';
    dream.dataset.statusFocus = JSON.stringify([project.id, 'dream']);
    dream.setAttribute('aria-label', `Dream now for ${project.id}`);
    dream.addEventListener('click', () => requestDream(project.id));
    actions.append(dream);
  }
  if (project.board_url) {
    const board = link('Open board', project.board_url, 'btn btn-sm btn-outline-secondary', [project.id, 'board']);
    if (board.href) board.setAttribute('aria-label', `Open board for ${project.id}`);
    actions.append(board);
  }
  if (actions.childElementCount) heading.append(actions);

  const body = element('div', undefined, 'card-body p-3');
  const worker = project.worker;
  const activity = element('div', undefined, 'd-flex flex-wrap align-items-center gap-2 mb-2');
  activity.append(activityStatus(worker, project.enabled));
  if (worker.started_at != null) {
    const timing = worker.finished_at == null ? `Started ${date(worker.started_at)}` : `Finished ${date(worker.finished_at)}`;
    activity.append(element('span', `${duration(worker.elapsed_seconds)} · ${timing}`, 'small text-secondary tabular-nums'));
  }
  body.append(activity);
  if (worker.last_error) body.append(element('p', worker.last_error, 'small text-danger-emphasis mb-2'));
  if (project.story) {
    const story = element('p', undefined, 'fw-semibold mb-2');
    story.append(project.story.url
      ? link(project.story.title, project.story.url, 'text-decoration-underline', [project.id, 'story', project.story.id])
      : element('span', project.story.title));
    body.append(story);
  }
  if (project.polling?.last_error) body.append(element('p', project.polling.last_error, 'small text-danger-emphasis mb-2'));

  const observation = project.observation;
  const reason = observation.reason;
  if (reason && !['no_eligible_work', 'occupied', 'work_selected'].includes(reason)) {
    const color = failedBoardActions.has(reason) ? 'text-danger-emphasis' : 'text-warning-emphasis';
    body.append(element('p', reasons[reason] || reason, `small ${color} mb-2`));
  }
  body.append(counts(observation.counts, true));

  // Use the pass start: receipt may be later than the board read or status writes.
  const observedAt = date(observation.read_started_at);
  const age = observation.counts == null
    ? 'Unknown · no complete board read'
    : observation.stale ? `Stale counts · Last successful read ${observedAt}` : `Observed · ${observedAt}`;
  const color = observation.stale ? 'text-warning-emphasis' : 'text-secondary';
  card.append(heading, body, element('footer', age, `card-footer small py-2 px-3 tabular-nums ${color}`));
  return card;
}
function render(snapshot) {
  lastSnapshot = snapshot;
  const daemon = document.getElementById('daemon');
  const workers = element('ul', undefined, 'row row-deck row-cols-1 row-cols-sm-2 row-cols-lg-3 g-2 g-sm-3 ps-0 mb-0');
  workers.append(agentRow('Coordinator', snapshot.coordinator));
  for (const project of snapshot.projects) {
    workers.append(agentRow(project.id, project.worker, project.enabled));
  }
  document.getElementById('uptime').textContent = `Uptime ${duration(snapshot.uptime_seconds)}`;
  daemon.replaceChildren(workers,
    element('p', `Last coordination pass: ${ago(snapshot.coordinator.started_at, snapshot.generated_at)}`, 'small text-secondary mt-2 mb-0'));
  document.getElementById('agents-section').hidden = false;
  const overall = snapshot.overall;
  const overview = document.getElementById('overall');
  overview.replaceChildren(counts(overall.counts));
  const coverage = [];
  if (overall.observed_projects < overall.total_projects) coverage.push(`Partial subtotal · ${overall.observed_projects}/${overall.total_projects} projects observed`);
  if (overall.stale) coverage.push('Stale counts');
  if (coverage.length) overview.append(element('p', coverage.join(' · '), 'small text-warning-emphasis mt-2 mb-0'));

  const projects = document.getElementById('projects');
  const focusedControl = document.activeElement?.dataset.statusFocus;
  const cards = snapshot.projects.map(projectCard);
  if (!cards.length) cards.push(element('p', 'No projects configured yet.', 'text-secondary mb-0'));
  projects.replaceChildren(...cards);
  document.getElementById('projects-section').hidden = false;
  if (focusedControl) {
    const replacement = Array.from(projects.querySelectorAll('[data-status-focus]'))
      .find(node => node.dataset.statusFocus === focusedControl);
    replacement?.focus({ preventScroll: true });
  }
}
let lastReceived = null;
function setConnection(color, state, detail) {
  const label = document.getElementById('connection-state');
  // Announce state transitions without announcing every successful poll.
  if (label.textContent !== state) {
    document.getElementById('connection').className = `status status-${color}`;
    label.textContent = state;
  }
  document.getElementById('connection-detail').textContent = detail;
}
async function refresh() {
  try {
    const response = await fetch('api/status', { cache: 'no-store', signal: AbortSignal.timeout(5000) });
    if (!response.ok) throw new Error('Status unavailable');
    render(await response.json());
    lastReceived = Date.now();
    setConnection('green', 'Connected', `Updated ${new Date(lastReceived).toLocaleTimeString()}`);
  } catch {
    const detail = lastReceived == null
      ? 'No status received yet.'
      : `Displayed information is stale. Last received ${new Date(lastReceived).toLocaleString()}.`;
    setConnection('red', 'Disconnected', `${detail} Retrying…`);
  } finally {
    setTimeout(refresh, 3000);
  }
}
refresh();
