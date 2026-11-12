#!/usr/bin/env python3
"""Simulated agents with a test-only external board, real local Git operations."""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__ROOT__)
args = sys.argv[1:]
claude = Path(sys.argv[0]).name == 'fake-claude'
if args == ['exec', '--help']:
    assert not claude
    print('--json --output-schema --ephemeral --ignore-user-config')
    sys.exit(0)
if args == ['login', 'status']:
    assert not claude
    sys.exit(0)
if args == ['--help']:
    assert claude
    print('--print --output-format --json-schema --model --no-session-persistence --strict-mcp-config --mcp-config --tools --allowedTools --disallowedTools --setting-sources --settings --permission-mode --dangerously-skip-permissions')
    sys.exit(0)
if args == ['auth', 'status']:
    assert claude
    print(json.dumps({'loggedIn': not (ROOT / 'logged-out').exists()}))
    sys.exit(0)
model = args[args.index('--model' if claude else '-m') + 1]
prompt = sys.stdin.read()
coordinator = '\nPROJECTS=' in prompt
defaults = ['haiku', 'opus'] if claude else ['gpt-6-luna', 'gpt-6-astra']
models = json.loads((ROOT / 'models.json').read_text()) if (ROOT / 'models.json').exists() else defaults
assert model == models[0 if coordinator else 1]
token = (ROOT / 'github-token').read_text().strip()
assert token not in prompt and not any(token in arg for arg in args)
if claude:
    assert '--print' in args and '--no-session-persistence' in args
    assert args[args.index('--output-format') + 1] == 'stream-json'
    assert '--verbose' in args and '--strict-mcp-config' in args
    assert json.loads(args[args.index('--settings') + 1])['disableAllHooks'] is True
    assert args[args.index('--disallowedTools') + 1] == 'Agent,Task'
    connections = json.loads(Path(args[args.index('--mcp-config') + 1]).read_text())['mcpServers']
    json.loads(args[args.index('--json-schema') + 1])
else:
    output = Path(args[args.index('-o') + 1])
    assert 'features.multi_agent=false' in args
    assert '--no-daemon' in args and '--ephemeral' in args
    json.loads(Path(args[args.index('--output-schema') + 1]).read_text())


def event(kind, **fields):
    with (ROOT / 'events').open('a') as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        file.write(json.dumps(dict(kind=kind, pid=os.getpid(), **fields)) + '\n')


def finish(result):
    if claude:
        if (ROOT / 'bad-result').exists():
            print(json.dumps({'type': 'result', 'subtype': 'error_max_turns', 'is_error': True}), flush=True)
        else:
            print(json.dumps({'type': 'result', 'subtype': 'success', 'is_error': False, 'structured_output': result}), flush=True)
    else:
        output.write_text(json.dumps(result))
        print(json.dumps({'type': 'turn.completed'}), flush=True)


def stopped(signum, frame):
    event('stopped', model=model)
    sys.exit(0)


signal.signal(signal.SIGTERM, stopped)
print(json.dumps({'type': 'thread.started', 'thread_id': str(os.getpid())}), flush=True)
(ROOT / ('coordinator-prompt.txt' if coordinator else Path.cwd().name + '-worker-prompt.txt')).write_text(prompt)
if coordinator:
    assert 'GH_TOKEN' not in os.environ and 'GITHUB_TOKEN' not in os.environ
    if claude:
        assert args[args.index('--setting-sources') + 1] == ''
        assert args[args.index('--tools') + 1] == ''
        assert args[args.index('--permission-mode') + 1] == 'dontAsk'
        assert '--dangerously-skip-permissions' not in args
    else:
        assert '--ignore-user-config' in args
        assert args[args.index('--sandbox') + 1] == 'read-only'
    # Detect simultaneous coordinator processes independently of the test board lock.
    singleton = (ROOT / 'coordinator.lock').open('w')
    fcntl.flock(singleton, fcntl.LOCK_EX | fcntl.LOCK_NB)
    snapshot = json.loads(prompt.split('\nSNAPSHOT=', 1)[1].splitlines()[0])
    projects = json.loads(prompt.split('\nPROJECTS=', 1)[1].split('\nSNAPSHOT=', 1)[0])
    configured = {arg.split('=', 1)[0].split('.', 1)[1] for arg in args
                  if arg.startswith('mcp_servers.') and '={' in arg}
    assert (set(connections) if claude else configured) == {p['mcp_server'] for p in projects}
    for project in projects:
        source = project['source']
        setting = (args[args.index('--allowedTools') + 1] if claude else
                   next(arg for arg in args if arg.startswith(f"mcp_servers.{project['mcp_server']}={{")))
        if claude:
            setting = ','.join(t for t in setting.split(',') if t.startswith(f"mcp__{project['mcp_server']}__"))
        if source == 'Shortcut':
            assert 'get_story' in setting and 'update_story' in setting
            assert 'update_row_cells' not in setting and 'create_story_comment' in setting
        else:
            assert source == 'AppFlowy' and 'update_row_cells' in setting
            assert 'update_story' not in setting
    event('coordinator', model=model, provider='claude' if claude else 'codex', snapshot=snapshot, projects=[p['id'] for p in projects])
    if (ROOT / 'luna-fail').exists():
        sys.exit(4)
    with (ROOT / 'board.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        board = json.loads((ROOT / 'board.json').read_text())
        result = dict(assignments=[], cancel=[], acknowledged=[], notes=[], observations=[], retry_projects=[])
        for project in projects:
            counts = dict(ready=0, in_progress=0, blocked=0, review=0, backlog=0, done=0)
            key = board[project['id']]['status'].lower().replace(' ', '_')
            if key in counts:
                counts[key] += 1
            result['observations'].append(dict(project_id=project['id'], counts=counts,
                                               read='complete', reason='no_eligible_work'))
        for active in snapshot['active']:
            if board[active['project_id']]['status'] == 'Cancelled':
                result['cancel'].append(active)
        # Simulate an unavailable AppFlowy write while a completion remains pending.
        if snapshot['pending'] and (ROOT / 'outage').exists():
            event('outage')
            sys.exit(4)
        for completion in snapshot['pending']:
            story = board[completion['story']['project_id']]
            if story['status'] == 'In Progress':
                story['status'] = completion['target_status']
                story['result'] = completion['report']
            result['acknowledged'].append(completion['id'])
            if (ROOT / 'queued-ready').exists():
                (ROOT / 'queued-ready').unlink()
                story['id'] = 'next-story'
                story['status'] = 'Ready'
        busy = {a['project_id'] for a in snapshot['active']}
        busy.update(c['story']['project_id'] for c in snapshot['pending'])
        for project in projects:
            story = board[project['id']]
            if project['id'] in busy or story['status'] not in ('Ready', 'In Progress'):
                continue
            result['assignments'].append(dict(project_id=project['id'], story_id=story['id'],
                                             title='Fixture story', story_url='https://appflowy.example/story', context='Complete fixture requirements',
                                             recovery=story['status'] == 'In Progress', pr=story.get('pr')))
            story['status'] = 'In Progress'
        (ROOT / 'board.json').write_text(json.dumps(board))
    if (ROOT / 'luna-incomplete').exists():
        result['retry_projects'] = [p['id'] for p in projects]
    if (ROOT / 'delivery-incomplete').exists():
        result['acknowledged'] = []
    event('coordinator_read')
    while (ROOT / 'luna-hold').exists():
        time.sleep(0.05)
    time.sleep(0.1)
    finish(result)
else:
    if claude:
        assert '--dangerously-skip-permissions' in args
        assert '--setting-sources' not in args
        assert set(connections) == {'utility'}
    else:
        assert '--ignore-user-config' not in args
        assert args[args.index('--sandbox') + 1] == 'danger-full-access'
        user_config = (ROOT / 'codex-home/config.toml').read_text()
        for server in ('appflowy', 'shortcut', 'shortcut_team'):
            assert (f'mcp_servers.{server}.enabled=false' in args) == (f'[mcp_servers.{server}]' in user_config)
        assert 'shell_environment_policy.ignore_default_excludes=true' in args
        assert 'shell_environment_policy.exclude=[]' in args
        assert 'shell_environment_policy.include_only=[]' in args
    assert os.environ['GIT_TERMINAL_PROMPT'] == '0'
    assert os.environ['GH_TOKEN'] == token and os.environ['GITHUB_TOKEN'] == token
    assert 'Use the supplied GH_TOKEN' in prompt
    # Exercise Git's actual credential lookup without a network request.
    credentials = subprocess.run(['git', 'credential', 'fill'],
        input='protocol=https\nhost=github.com\n\n', text=True, capture_output=True)
    assert credentials.returncode == 0
    assert f'password={token}' in credentials.stdout
    assignment = json.loads(prompt.split('\nASSIGNMENT=', 1)[1].splitlines()[0])
    git_config = json.loads(prompt.split('\nGIT=', 1)[1].split('\nASSIGNMENT=', 1)[0])
    project = assignment['project_id']
    assert Path.cwd() == ROOT / project
    event('worker_start', model=model, provider='claude' if claude else 'codex', project=project, recovery=assignment['recovery'], git=git_config)
    if git_config['mode'] == 'pr':
        assert 'PR MODE:' in prompt and 'MASTER MODE:' not in prompt
    else:
        assert 'MASTER MODE:' in prompt and 'PR MODE:' not in prompt
    if (ROOT / 'hang').exists():
        while True:
            time.sleep(0.05)
    # Both project workers must have started before either may complete.
    if (ROOT / 'parallel').exists():
        deadline = time.monotonic() + 10
        while len({json.loads(l).get('project') for l in (ROOT / 'events').read_text().splitlines()
                   if json.loads(l)['kind'] == 'worker_start'}) < 2:
            assert time.monotonic() < deadline, 'project workers did not run concurrently'
            time.sleep(0.05)
    def git(*command):
        return subprocess.check_output(['git', *command], text=True, stderr=subprocess.PIPE).strip()
    branch = git_config['story_branch'] if git_config['mode'] == 'pr' else git_config['base_branch']
    existing = subprocess.run(['git', 'show-ref', '--verify', '--quiet', f'refs/heads/{branch}']).returncode == 0
    git('checkout', branch) if existing else git('checkout', '-b', branch, git_config['base_branch'])
    if git_config['story_marker'] not in git('log', '-1', '--format=%B'):
        Path('result.txt').write_text('implemented\n')
        # A real test is performed by the worker, not by the daemon.
        subprocess.check_call([sys.executable, '-c', "assert open('result.txt').read() == 'implemented\\n'"])
        git('add', 'result.txt')
        git('commit', '--allow-empty', '-m', git_config['story_marker'])
    git('push', git_config['remote'], branch)
    sha = git('rev-parse', 'HEAD')
    result = dict(outcome='completed', summary='Implemented fixture', tests=['python assertion passed'],
                  commit=sha, pr_url=f'https://example.test/pull/{project}' if git_config['mode'] == 'pr' else None)
    if (ROOT / 'missing-pr-url').exists():
        result['pr_url'] = None
    event('worker_finish', project=project)
    finish(result)
