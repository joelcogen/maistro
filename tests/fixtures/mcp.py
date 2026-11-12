#!/usr/bin/env python3
"""Read-only MCP fixture using AppFlowy's legacy response shapes, no model/API calls."""
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
        return dict(view_id='view-' + project, name='board', layout=2, extra=story.get('metadata'), request_id=sequence)
    if name == 'get_database_fields':
        fields = story.get('fields', [{'id': 'title', 'name': 'Title'}, {'id': 'status', 'name': 'Status'}])
        return fields if sequence % 2 else list(reversed(fields))
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


for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    try:
        method = request['method']
        if method == 'initialize':
            result = dict(protocolVersion=request['params']['protocolVersion'], capabilities={'tools': {}},
                          serverInfo=dict(name='fixture-appflowy', version='1'))
        elif method == 'tools/call':
            args = request['params']
            data = call(args['name'], args.get('arguments', {}))
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
