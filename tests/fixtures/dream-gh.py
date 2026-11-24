#!/usr/bin/env python3
"""Simulated GitHub CLI for the daily dream publication workflow."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys

repo = Path.cwd()
root = repo.parent
token = (root / 'github-token').read_text().strip()
assert os.environ['GH_TOKEN'] == os.environ['GITHUB_TOKEN'] == token
assert all(token not in arg for arg in sys.argv)
assert sys.argv[1:4] == ['api', '--hostname', 'github.com']
args = sys.argv[4:]
project = repo.name
assert project in ('p0', 'p1', 'p4')
with (root / 'events').open('a+') as file:
    fcntl.flock(file, fcntl.LOCK_EX)
    file.seek(0)
    events = list(map(json.loads, file))
    assert [e['name'] for e in events if e['kind'] == 'dream_finish' and e['project'] == project] == [
        '01-first.md', '02-second.md', '03-noop.md'
    ]
    method = args[args.index('--method') + 1]
    file.write(json.dumps(dict(kind='dream_github', project=project, method=method)) + '\n')

base = 'release/main'
pulls = f'repos/fixture/{project}/pulls'
url = f'https://github.com/fixture/{project}/pull/1'
if method == 'POST':
    assert project == 'p0'
    assert args == ['--method', 'POST', pulls, '--jq', '.html_url', '-f', 'head=dreams',
                    '-f', f'base={base}', '-f', 'title=Daily dreams',
                    '-f', "body=Changes from Maistro's daily dream phases."]
    remote = root / f'remote{project[1:]}.git'
    tip = subprocess.check_output(['git', '-C', remote, 'log', '-2', '--format=%s', 'dreams'], text=True)
    assert tip.splitlines() == ['Dream: 02-second.md', 'Dream: 01-first.md']
    print(url)
elif pulls in args:
    assert args == ['--method', 'GET', pulls, '--jq', '.[0].html_url // empty', '-f',
                    'state=open', '-f', 'head=fixture:dreams', '-f', f'base={base}']
    if project == 'p1':
        print(url)
else:
    assert project in ('p0', 'p4')
    assert args == ['--method', 'GET', f'/repos/fixture/{project}/compare/release%2Fmain...dreams',
                    '--jq', '.ahead_by', '-f', 'per_page=1']
    print(2 if project == 'p0' else 0)
