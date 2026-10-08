from contextlib import closing
import base64
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import Mock

HELPER = Path(__file__).parents[1] / 'src-tauri/src/auth/t3_sync.py'
spec = importlib.util.spec_from_file_location('t3_sync', HELPER)
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)


class SwitchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.auth = self.home / 'auth.json'
        self.old = {'tokens': {'account_id': 'old', 'access_token': 'old'}}
        self.new = {'tokens': {'account_id': 'new', 'access_token': 'new'}}
        self.auth.write_text(json.dumps(self.old))
        self.db = self.home / 'state.sqlite'
        with closing(sqlite3.connect(self.db, isolation_level=None)) as db:
            db.execute('create table orchestration_v2_projection_runs (status text)')
        self.service = Mock()
        self.service.start.side_effect = lambda: None
        self.service.verify.side_effect = lambda auth: None

    def insert(self, status):
        with closing(sqlite3.connect(self.db, isolation_level=None)) as db:
            db.execute('insert into orchestration_v2_projection_runs values (?)', (status,))

    def apply(self):
        return sync.apply(self.auth, self.db, self.service, self.new, sync.revision(self.auth))

    def test_idle_switch_stops_whole_service_before_writing_and_verifies(self):
        self.insert('completed')
        self.service.stop.side_effect = lambda: self.assertEqual(json.loads(self.auth.read_text()), self.old)
        self.service.start.side_effect = lambda: self.assertEqual(json.loads(self.auth.read_text()), self.new)
        self.apply()
        self.service.stop.assert_called_once()
        self.service.start.assert_called_once()
        self.service.verify.assert_called_once_with(self.new)
        self.assertEqual(self.auth.stat().st_mode & 0o777, 0o600)

    def test_running_or_waiting_work_blocks_without_writing_or_stopping(self):
        for status in ['running', 'queued', 'waiting_for_approval', 'unknown']:
            with self.subTest(status=status):
                with closing(sqlite3.connect(self.db, isolation_level=None)) as db:
                    db.execute('delete from orchestration_v2_projection_runs')
                self.insert(status)
                with self.assertRaises(sync.SyncError): self.apply()
                self.assertEqual(json.loads(self.auth.read_text()), self.old)
                self.service.stop.assert_not_called()

    def test_run_started_during_stop_aborts_before_auth_write(self):
        self.service.stop.side_effect = lambda: self.insert('interrupted')
        with self.assertRaises(sync.SyncError): self.apply()
        self.assertEqual(json.loads(self.auth.read_text()), self.old)
        self.service.start.assert_called_once()

    def test_uncertain_stop_recovers_service_without_changing_auth(self):
        import subprocess
        self.service.stop.side_effect = subprocess.TimeoutExpired('systemctl', 40)
        with self.assertRaises(sync.SyncError): self.apply()
        self.assertEqual(json.loads(self.auth.read_text()), self.old)
        self.service.start.assert_called_once()

    def test_failed_restart_restores_previous_account(self):
        self.service.verify.side_effect = RuntimeError('private provider response')
        with self.assertRaises(sync.SyncError) as error: self.apply()
        self.assertNotIn('private provider response', str(error.exception))
        self.assertEqual(json.loads(self.auth.read_text()), self.old)

    def test_stale_probe_never_overwrites_newer_login(self):
        before = sync.revision(self.auth)
        self.auth.write_text(json.dumps(self.new))
        with self.assertRaises(sync.SyncError):
            sync.apply(self.auth, self.db, self.service, self.old, before)
        self.assertEqual(json.loads(self.auth.read_text()), self.new)
        self.service.stop.assert_not_called()

    def test_fresh_cache_from_wrong_account_is_not_confirmation(self):
        payload = base64.urlsafe_b64encode(json.dumps({'email':'new@example.com'}).encode()).decode().rstrip('=')
        auth = {'tokens': {'id_token': 'header.' + payload + '.signature'}}
        self.assertFalse(sync.cache_matches({'auth': {'email':'old@example.com'}}, auth))
        self.assertTrue(sync.cache_matches({'auth': {'email':'new@example.com'}}, auth))

    def test_missing_database_and_auth_symlink_fail_closed(self):
        self.db.unlink()
        with self.assertRaises(sync.SyncError): self.apply()
        self.service.stop.assert_not_called()
        target = self.home / 'target.json'
        self.auth.rename(target)
        self.auth.symlink_to(target)
        with self.assertRaises(sync.SyncError): sync.revision(self.auth)


if __name__ == '__main__': unittest.main()
