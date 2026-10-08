"""Private SSH helper. Never log input/output: probe replies contain credentials."""
from contextlib import closing
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time
import tomllib


class SyncError(Exception):
    pass


def revision(path):
    if path.is_symlink() or not path.is_file():
        raise SyncError('Remote credential file is missing or linked; switch cancelled.')
    return hashlib.sha256(path.read_bytes()).hexdigest()


def idle_watermark(database):
    if not database.is_file():
        raise SyncError('Cannot check T3 activity; switch cancelled.')
    try:
        with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True, timeout=5)) as db:
            rows = db.execute('SELECT rowid, status FROM orchestration_v2_projection_runs').fetchall()
    except sqlite3.Error:
        raise SyncError('Cannot read T3 activity; switch cancelled.') from None
    if any(status not in ('completed', 'failed', 'cancelled', 'interrupted') for _, status in rows):
        raise SyncError('T3 has running work or a pending approval. Finish or stop it, then switch again.')
    return max((row for row, _ in rows), default=0)


def write_auth(path, data):
    if path.is_symlink():
        raise SyncError('Remote credential path changed; switch cancelled.')
    fd, name = tempfile.mkstemp(prefix='.tracker-auth-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name): os.unlink(name)


def apply(path, database, service, auth, expected_revision):
    if revision(path) != expected_revision:
        raise SyncError('Harness login changed during the switch. Retry to use its latest credentials.')
    watermark = idle_watermark(database)
    previous = path.read_bytes()
    try:
        service.stop()
    except Exception:
        try:
            service.start()
            service.ensure_running()
        except Exception:
            raise SyncError('T3 stop failed and service recovery needs attention. Credentials were not changed.') from None
        raise SyncError('T3 could not stop safely; service restarted without changing credentials.') from None
    # Detect a scheduled/user turn that raced with the initial idle check.
    try:
        if idle_watermark(database) != watermark or revision(path) != expected_revision:
            raise SyncError('T3 activity or login changed while stopping. Switch cancelled; retry.')
    except Exception:
        service.start()
        raise
    try:
        write_auth(path, json.dumps(auth).encode())
        service.start()
        service.verify(auth)
    except Exception:
        try:
            service.stop()
            write_auth(path, previous)
            service.start()
            service.ensure_running()
        except Exception:
            raise SyncError('T3 restart failed and recovery needs attention. Check harness before switching again.') from None
        raise SyncError('T3 did not restart with the selected account. Previous harness login restored.') from None
    return {'restarted': True}


class Service:
    def __init__(self, root):
        self.runtime = root / '.t3/userdata/server-runtime.json'
        self.cache = root / '.t3/caches/codex.json'
        self.old_pid = json.loads(self.runtime.read_text())['pid']
        self.started = 0
        result = subprocess.run(['systemctl', '--user', 'is-active', '--quiet', 't3code.service'], capture_output=True, timeout=10)
        if result.returncode:
            raise SyncError('The managed T3 service is not running on harness; switch cancelled.')

    def command(self, action):
        result = subprocess.run(['systemctl', '--user', action, 't3code.service'], capture_output=True, timeout=40)
        if result.returncode:
            raise SyncError('Could not ' + action + ' the managed T3 service.')

    def stop(self): self.command('stop')

    def start(self):
        self.started = time.time()
        self.command('start')

    def ensure_running(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                runtime = json.loads(self.runtime.read_text())
                os.kill(runtime['pid'], 0)
                result = subprocess.run(['systemctl', '--user', 'is-active', '--quiet', 't3code.service'], capture_output=True, timeout=5)
                if result.returncode == 0: return
            except (OSError, ValueError, KeyError):
                pass
            time.sleep(0.25)
        raise SyncError('T3 did not recover after the cancelled switch.')

    def verify(self, auth):
        from datetime import datetime
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                runtime = json.loads(self.runtime.read_text())
                cache = json.loads(self.cache.read_text())
                checked = datetime.fromisoformat(cache['checkedAt'].replace('Z', '+00:00')).timestamp()
                current = json.loads((self.runtime.parents[2] / '.codex/auth.json').read_text())
                if (runtime['pid'] != self.old_pid and checked >= self.started
                        and cache['status'] == 'ready' and cache['auth']['status'] == 'authenticated'
                        and identity(current) == identity(auth) and cache_matches(cache, auth)):
                    os.kill(runtime['pid'], 0)
                    return
            except (OSError, ValueError, KeyError):
                pass
            time.sleep(0.25)
        raise SyncError('T3 did not confirm its new account before the timeout.')


def cache_matches(cache, auth):
    tokens = auth.get('tokens') or {}
    if tokens:
        try:
            payload = tokens['id_token'].split('.')[1]
            claims = json.loads(base64.urlsafe_b64decode(payload + '=' * (-len(payload) % 4)))
            return bool(claims.get('email')) and cache['auth'].get('email') == claims['email']
        except (KeyError, ValueError, IndexError):
            return False
    return cache['auth'].get('type') == 'apiKey'


def identity(auth):
    tokens = auth.get('tokens') or {}
    if tokens.get('account_id'): return ('chatgpt', tokens['account_id'])
    if auth.get('OPENAI_API_KEY'): return ('api', auth['OPENAI_API_KEY'])
    raise SyncError('Unsupported credentials; switch cancelled.')


def run(request):
    root = Path.home()
    home = root / '.codex'
    path = home / 'auth.json'
    database = root / '.t3/userdata/statev2.sqlite'
    config = tomllib.loads((home / 'config.toml').read_text())
    if config.get('cli_auth_credentials_store', 'auto') != 'file' or config.get('profile') or config.get('model_provider', 'openai') != 'openai':
        raise SyncError('Harness must use the default provider and file login storage for T3 account sync.')
    # Refuse an alternate T3 home: writing default auth would not switch it.
    cache = json.loads((root / '.t3/caches/codex.json').read_text())
    if cache.get('continuation', {}).get('groupKey') != 'codex:home:' + str(home):
        raise SyncError('T3 uses another Codex home. Switch cancelled.')
    fd = os.open(home / '.switcher-sync.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        try: fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: raise SyncError('Another harness switch is in progress.') from None
        service = Service(root)
        idle_watermark(database)
        if request['action'] == 'probe':
            return {'auth': json.loads(path.read_text()), 'revision': revision(path)}
        if request['action'] != 'apply': raise SyncError('Unknown sync action.')
        identity(request['auth'])
        return apply(path, database, service, request['auth'], request['revision'])


if __name__ == '__main__':
    try:
        print(json.dumps({'result': run(json.load(sys.stdin))}))
    except SyncError as error:
        print(json.dumps({'error': str(error)}))
        sys.exit(1)
    except Exception:
        print(json.dumps({'error': 'Harness sync failed. Check SSH, T3 service, and its account settings.'}))
        sys.exit(1)
