#!/usr/bin/env python3
"""Read-only MCP fixture using AppFlowy's compact request and response shapes, no model/API calls."""
import hashlib
import fcntl
import json
from pathlib import Path
import sys
import time
import uuid

ROOT = Path(sys.argv[1])
sequence = 0


def event(name, **fields):
    with (ROOT / 'poll-events').open('a') as out:
        fcntl.flock(out, fcntl.LOCK_EX)
        out.write(json.dumps(dict(tool=name, **fields)) + '\n')


def call(name, args):
    global sequence
    sequence += 1
    with (ROOT / 'board.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        board = json.loads((ROOT / 'board.json').read_text())
    project = args.get('database_id', args.get('view_id', ''))
    if project.startswith('view-'):
        project = project[5:]
    event(name, project=project)
    if (ROOT / ('poll-fail-' + project)).exists():
        raise RuntimeError('simulated read failure')
    if (ROOT / ('poll-slow-' + project)).exists():
        time.sleep(20)
    if name == 'get_workspaces':
        return [{'workspace_id': 'workspace'}]
    if name == 'list_databases':
        return [{'id': p, 'views': [{'view_id': 'view-' + p, 'name': 'board-' + p[1:]}]}
                for p in board]
    story = board.get(project, {})
    if name == 'get_page':
        return dict(view=dict(view_id='view-' + project, name='board', layout=2, extra=story.get('metadata')), request_id=sequence)
    if name == 'get_database_fields':
        fields = story.get('fields', [{'id': 'title', 'name': 'Title'}, {'id': 'status', 'name': 'Status'}])
        return list(reversed(fields))
    if name == 'get_database_row_ids':
        if (ROOT / ('poll-paged-' + project)).exists():
            return {'items': [], 'has_more': True}
        return [{'id': s['id']} for s in story.get('rows', [story])]
    if name == 'get_database_row_details':
        if (ROOT / ('poll-incomplete-' + project)).exists():
            return []
        return [dict(id=s['id'], cells=dict(title=sequence if (ROOT / ('poll-churn-' + project)).exists() else s.get('title', 'Story'), status=s.get('status', 'Backlog'), extra=s.get('cell')),
                     has_doc='body' in s, doc=None)
                for s in reversed(story.get('rows', [story])) if s['id'] in args['row_ids'].split(',')]
    for p, s in board.items():
        for row in s.get('rows', [s]):
            target = args.get('target_id', args.get('page_id'))
            inline = args.get('target_type') == 'document'
            if inline:
                matches = target == str(uuid.uuid5(uuid.UUID(row['id']), 'document_id'))
            else:
                matches = target == row['id']
            if matches:
                if name == 'get_page_markdown':
                    if (ROOT / ('poll-body-error-' + p)).exists():
                        raise RuntimeError('body unavailable')
                    return row['body']
                if name == 'list_comments':
                    if (ROOT / ('poll-bad-comments-' + p)).exists():
                        return dict(comments=[], has_more=True, next_offset=0)
                    comments = row.get('inline_comments' if inline else 'comments', [])
                    # Force small pages to exercise continuation on normal data.
                    offset, limit = args['offset'], 2
                    return dict(comments=comments[offset:offset + limit], has_more=offset + limit < len(comments),
                                next_offset=offset + limit if offset + limit < len(comments) else None)
    raise RuntimeError('unexpected fixture call ' + name)


def compact(name, arguments):
    assert set(arguments) == {'request'}
    r = arguments['request']
    args = dict(r)
    if name == 'appflowy_fetch':
        kind = r.get('kind', 'page')
        old = {'workspaces': 'get_workspaces', 'comments': 'list_comments', 'page': 'get_page', 'workspace_folder': 'get_page'}[kind]
        if r.get('response_format') == 'markdown':
            old = 'get_page_markdown'
        args.update(view_id=r.get('id', ''), target_id=r.get('id', ''), page_id=r.get('id', ''))
    elif name == 'appflowy_database':
        old = {'list': 'list_databases', 'fields': 'get_database_fields'}[r['command']]
    elif name == 'appflowy_rows':
        assert r['command'] == 'list' and r['with_doc'] is False
        ids = call('get_database_row_ids', args)
        if isinstance(ids, dict):
            return ids
        args['row_ids'] = ','.join(x['id'] for x in ids)
        data = call('get_database_row_details', args)
        if len(data) != len(ids):
            data = [{'id': x['id'], 'unavailable': True} for x in ids]
        old = None
    else:
        raise RuntimeError('legacy tools are not exposed')
    if old:
        data = call(old, args)
    if old == 'list_comments':
        return data
    if old == 'get_page' and r['kind'] == 'page':
        return {'truncated': True, 'preview': 'oversized embedded board data'}
    if old == 'get_page':
        return {'data': data, 'truncated': False}
    if old == 'get_page_markdown':
        offset = r.get('offset', 0)
        end = min(offset + 3, len(data))
        return dict(data={'markdown': data[offset:end]}, offset=offset,
                    next_offset=end if end < len(data) else None, has_more=end < len(data),
                    truncated=end < len(data), content_hash=('changed' if offset and (ROOT / 'poll-bad-hash').exists() else hashlib.sha256(data.encode()).hexdigest()))
    offset = r.get('offset', 0)
    end = min(offset + 2, len(data))
    return {('data' if old == 'get_workspaces' else 'items'): data[offset:end],
            'offset': offset, 'has_more': end < len(data),
            'next_offset': end if end < len(data) else None}


for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    try:
        method = request['method']
        if method == 'initialize':
            result = dict(protocolVersion=request['params']['protocolVersion'], capabilities={'tools': {}},
                          serverInfo=dict(name='fixture-appflowy', version='1'))
        elif method == 'tools/list':
            result = {'tools': []} if (ROOT / 'legacy-profile').exists() else {'tools': [dict(name=n, inputSchema={'type': 'object'}) for n in ['appflowy_fetch', 'appflowy_database', 'appflowy_rows', 'appflowy_update']]}
        elif method == 'tools/call':
            args = request['params']
            data = compact(args['name'], args.get('arguments', {}))
            result = dict(content=[dict(type='text', text=data if isinstance(data, str) else json.dumps(data))],
                          structuredContent={'result': data} if not isinstance(data, dict) else data,
                          isError=False, _meta={'request_id': sequence})
        elif method == 'ping':
            result = {}
        else:
            raise RuntimeError('unexpected MCP method')
    except Exception as error:
        result = dict(content=[dict(type='text', text=str(error))], isError=True)
    print(json.dumps(dict(jsonrpc='2.0', id=request['id'], result=result)), flush=True)
