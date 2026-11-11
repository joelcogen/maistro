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
if args == ['exec', '--help']:
    print('--json --output-schema --ephemeral --ignore-user-config')
    sys.exit(0)
if args == ['login', 'status']:
    sys.exit(0)
model = args[args.index('-m') + 1]
prompt = sys.stdin.read()
token = (ROOT / 'github-token').read_text().strip()
assert token not in prompt and not any(token in arg for arg in args)
output = Path(args[args.index('-o') + 1])
assert 'features.multi_agent=false' in args
assert '--no-daemon' in args and '--ephemeral' in args
json.loads(Path(args[args.index('--output-schema') + 1]).read_text())


def event(kind, **fields):
    with (ROOT / 'events').open('a') as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        file.write(json.dumps(dict(kind=kind, pid=os.getpid(), **fields)) + '\n')


def finish(result):
    output.write_text(json.dumps(result))
    print(json.dumps({'type': 'turn.completed'}), flush=True)


def stopped(signum, frame):
    event('stopped', model=model)
    sys.exit(0)


signal.signal(signal.SIGTERM, stopped)
print(json.dumps({'type': 'thread.started', 'thread_id': str(os.getpid())}), flush=True)
if model == 'gpt-6-luna':
    assert 'GH_TOKEN' not in os.environ and 'GITHUB_TOKEN' not in os.environ
    assert '--ignore-user-config' in args
    assert args[args.index('--sandbox') + 1] == 'read-only'
    # Detect simultaneous coordinator processes independently of the test board lock.
    singleton = (ROOT / 'coordinator.lock').open('w')
    fcntl.flock(singleton, fcntl.LOCK_EX | fcntl.LOCK_NB)
    snapshot = json.loads(prompt.split('\nSNAPSHOT=', 1)[1])
    projects = json.loads(prompt.split('\nPROJECTS=', 1)[1].split('\nSNAPSHOT=', 1)[0])
    event('coordinator', snapshot=snapshot)
    with (ROOT / 'board.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        board = json.loads((ROOT / 'board.json').read_text())
        result = dict(assignments=[], cancel=[], acknowledged=[], notes=[], observations=[])
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
                story['status'] = ('Blocked' if completion['report']['outcome'] == 'blocked'
                                   else 'Review' if story['mode'] == 'pr' else 'Done')
                story['result'] = completion['report']
            result['acknowledged'].append(completion['id'])
        busy = {a['project_id'] for a in snapshot['active']}
        busy.update(c['story']['project_id'] for c in snapshot['pending'])
        for project in projects:
            story = board[project['id']]
            if project['id'] in busy or story['status'] not in ('Ready', 'In Progress'):
                continue
            result['assignments'].append(dict(project_id=project['id'], story_id=story['id'],
                                             title='Fixture story', story_url='https://appflowy.example/story', context='Complete fixture requirements',
                                             recovery=story['status'] == 'In Progress'))
            story['status'] = 'In Progress'
        (ROOT / 'board.json').write_text(json.dumps(board))
    time.sleep(0.1)
    finish(result)
else:
    assert model == 'gpt-6-astra'
    assert '--ignore-user-config' not in args
    assert args[args.index('--sandbox') + 1] == 'danger-full-access'
    assert 'mcp_servers.appflowy.enabled=false' in args
    assert os.environ['GIT_TERMINAL_PROMPT'] == '0'
    assert os.environ['GH_TOKEN'] == token and os.environ['GITHUB_TOKEN'] == token
    assert 'shell_environment_policy.ignore_default_excludes=true' in args
    assert 'shell_environment_policy.exclude=[]' in args
    assert 'shell_environment_policy.include_only=[]' in args
    assert 'Use the supplied GH_TOKEN' in prompt
    # Exercise Git's actual credential lookup without a network request.
    credentials = subprocess.run(['git', 'credential', 'fill'],
        input='protocol=https\nhost=github.com\n\n', text=True, capture_output=True)
    assert credentials.returncode == 0
    assert f'password={token}' in credentials.stdout
    assert 'PROJECT INSTRUCTIONS:\nRun fixture tests.' in prompt
    assignment = json.loads(prompt.split('\nASSIGNMENT=', 1)[1])
    git_config = json.loads(prompt.split('\nGIT=', 1)[1].split('\nASSIGNMENT=', 1)[0])
    project = assignment['project_id']
    assert Path.cwd() == ROOT / project
    event('worker_start', project=project, recovery=assignment['recovery'])
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
        git('commit', '-m', git_config['story_marker'])
    git('push', git_config['remote'], branch)
    sha = git('rev-parse', 'HEAD')
    result = dict(outcome='completed', summary='Implemented fixture', tests=['python assertion passed'],
                  commit=sha, pr_url=f'https://example.test/pull/{project}' if git_config['mode'] == 'pr' else None)
    event('worker_finish', project=project)
    finish(result)
