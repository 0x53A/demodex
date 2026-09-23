"""Test-only fixture shorthand, executed by the real native Wormhole client.

Paths here describe existing test scenarios; no HTTP API or compatibility server
is involved. Each invocation opens a new actor connection, exercising reconnects.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlsplit, parse_qs

ROOT = Path(__file__).resolve().parents[1]


def operation(path, body):
    url = urlsplit(path)
    parts = url.path.strip('/').split('/')
    if len(parts) == 1:
        read, write = {
            'sessions': ('Sessions', 'ExternalSession'),
            'runtime': ('Runtime', None),
            'targets': ('Targets', 'RegisterTarget'),
            'environments': ('Environments', 'CreateEnvironment'),
        }[parts[0]]
        return read if body is None else {write: {'input': body}}
    if parts == ['host', 'sessions']:
        return {'HostSession': {'input': body}}
    if parts == ['runtime', 'sessions']:
        return {'CreateSession': {'input': body}}
    if parts == ['runtime', 'start']:
        return 'StartRuntime'
    if parts == ['runtime', 'login']:
        return 'Login'
    if parts == ['targets', 'ssh']:
        return {'RegisterSshTarget': {'input': body}}
    kind, identifier, *action = parts
    action = action[0] if action else ''
    data = {'id': identifier}
    if kind == 'sessions':
        name = {'': 'Detail', 'events': 'Events', 'connect': 'Connect', 'archive': 'Archive',
                'sandbox': 'Sandbox', 'models': 'Models', 'model': 'Model', 'goal': 'Goal',
                'messages': 'Prompt', 'queue': 'QueuePrompt', 'interrupt': 'Interrupt',
                'answer': 'Answer', 'targets': 'SelectTargets'}[action]
        if action == 'events':
            data['after'] = int(parse_qs(url.query).get('after', ['0'])[0])
        elif action in ('sandbox', 'model', 'goal', 'targets'):
            data['input'] = body
        elif action in ('archive', 'messages', 'queue', 'answer'):
            data.update(body)
    elif kind == 'targets':
        name = {'check': 'CheckSshTarget', 'reconnect': 'ReconnectSshTarget', 'forget': 'ForgetTarget'}[action]
    elif kind == 'environments':
        name = {'start': 'StartEnvironment', 'stop': 'StopEnvironment', 'sessions': 'EnvironmentSession'}[action]
        if action == 'sessions':
            data['input'] = body
    else:
        raise AssertionError(f'Unknown fixture operation: {path}')
    return {name: data}


def call(origin, token, command, request_id=None):
    binary = os.environ.get('DEMODEX_BIN', str(ROOT / 'target/rust-pwa/debug/demodex'))
    url = origin.replace('http://', 'ws://').replace('https://', 'wss://').rstrip('/') + '/wormhole'
    with tempfile.NamedTemporaryFile(mode='w') as credential:
        credential.write(token)
        credential.flush()
        result = subprocess.run([binary, 'call', '--url', url, '--token-file', credential.name],
            input=json.dumps({'request_id': request_id, 'operation': command})+'\n',
            text=True, capture_output=True, timeout=135)
    assert result.returncode == 0, result.stderr
    response = json.loads(result.stdout)
    if 'Err' in response:
        raise AssertionError(response['Err'])
    return response['Ok']


def api(origin, token, path, body=None, error=None):
    try:
        value = call(origin, token, operation(path, body))
    except AssertionError as exc:
        if error is None:
            raise
        assert error.casefold() in str(exc).casefold(), str(exc)
        return None
    assert error is None, value
    return value
