#!/usr/bin/env python3
"""Vaultite's local stdio text MCP and guarded Markdown file edits."""
import fcntl
import json
from pathlib import Path
import sys

ROOT = Path(__file__).parent
assert sys.argv[1:] == ['mcp']


def page(text, offset):
    lines = text.splitlines(keepends=True)
    tail = ''.join(lines[offset - 1:])
    guidance = '; to change it: edit_file, or write_file with base)'
    if len(tail) <= 100_000:
        if offset == 1:
            return tail
        end = len(lines)
        span = f'line {end}' if offset == end else f'lines {offset}-{end}'
        return tail.removesuffix('\n') + f'\n\n({span} of {end}{guidance}'
    newline = tail.rfind('\n', 0, 100_000) + 1
    assert newline > 0
    chunk = tail[:newline]
    end = offset - 1 + len(chunk.splitlines())
    span = f'line {end}' if offset == end else f'lines {offset}-{end}'
    return chunk + f'\n({span} of {len(lines)}; more: offset {end + 1}{guidance}'


def cell(text, name):
    frontmatter = text.split('---\n', 2)[1]
    value = next((line.split(':', 1)[1].strip() for line in frontmatter.splitlines()
                  if line.startswith(name + ':')), '')
    return json.loads(value) if value.startswith('"') else value


def query(arguments):
    rows = []
    for folder in arguments['from']:
        for file in sorted((ROOT / folder).rglob('*.md')):
            text = file.read_text()
            if cell(text, 'archived') == 'true':
                continue
            rows.append((str(file.relative_to(ROOT)), cell(text, 'id'), cell(text, 'status')))
    assert arguments['offset'] == 0 and len(rows) <= arguments['limit']
    body = '| Path | Id | Status |\n| --- | --- | --- |\n' + '\n'.join(
        '| ' + ' | '.join(row) + ' |' for row in rows) if rows else '_None._'
    return f'{len(rows)} files.\n\n{body}'


def call(tool, arguments):
    if tool == 'query':
        return query(arguments)
    path = arguments['path']
    file = (ROOT / path).resolve()
    assert file.is_relative_to(ROOT) and file.suffix in ('.md', '.base')
    with (ROOT / 'vaultite.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        current = file.read_text()
        if tool == 'read':
            return path + '\n\n' + page(current, arguments.get('offset', 1))
        assert tool == 'edit_file'
        before, after = arguments['old'], arguments['new']
        race = ROOT / 'vaultite-edit-race'
        if race.exists():
            race.unlink()
            current = current.replace('Quote commas.', 'Quote commas and preserve empty cells.')
            file.write_text(current)
        if current.count(before) != 1:
            raise RuntimeError('old text does not match exactly once')
        file.write_text(current.replace(before, after, 1))
        lost = ROOT / 'vaultite-lose-completion-reply'
        if 'maistro-completion:' in after and lost.exists():
            lost.rename(ROOT / 'vaultite-completion-reply-lost')
            raise RuntimeError('simulated lost reply after a successful edit')
        return f'Edited {path}.'


for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        result = dict(protocolVersion=request['params']['protocolVersion'], capabilities={'tools': {}},
                      serverInfo=dict(name='fixture-vaultite', version='1'))
    elif method == 'tools/list':
        result = {'tools': [dict(name=name, inputSchema={'type': 'object'})
                            for name in ['query', 'read', 'edit_file']]}
    elif method == 'tools/call':
        params = request['params']
        try:
            result = dict(content=[dict(type='text', text=call(params['name'], params['arguments']))])
        except Exception as error:
            result = dict(content=[dict(type='text', text=str(error))], isError=True)
    else:
        assert method == 'ping'
        result = {}
    print(json.dumps(dict(jsonrpc='2.0', id=request['id'], result=result)), flush=True)
