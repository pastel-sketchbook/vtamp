"""Checks for the two capture operations that must preserve existing user data."""
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("capture_site", Path(__file__).with_name("capture-site.py"))
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


class CaptureTests(unittest.TestCase):
    def test_live_snapshot_preserves_source_and_uses_current_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source with spaces"
            destination = root / "copy"
            source.mkdir()
            destination.mkdir()
            cover = root / "cover.png"
            cover.write_bytes(b"cover")
            saved = {"queue": [{"id": "one", "track": {"cover": str(cover)}}],
                     "current_id": "one", "status": "playing", "position_ms": 1000}
            live = dict(saved, position_ms=42000, scanning=True, last_error="old error")
            with sqlite3.connect(source / "state.db") as database:
                database.executescript("CREATE TABLE session(id INTEGER PRIMARY KEY,json TEXT);"
                                       "CREATE TABLE tracks(id TEXT); INSERT INTO tracks VALUES('one');")
                database.execute("INSERT INTO session VALUES(1,?)", (json.dumps(saved),))
            before = (source / "state.db").read_bytes()

            def response(args):
                data = {"data_directory": str(source), "server_reachable": True} if args[1] == "doctor" else live
                return json.dumps({"data": data})

            with patch.object(capture, "run", side_effect=response):
                capture.snapshot("vtamp", destination)
            with sqlite3.connect(destination / "state.db") as database:
                copied = json.loads(database.execute("SELECT json FROM session").fetchone()[0])
                self.assertEqual(database.execute("SELECT id FROM tracks").fetchall(), [("one",)])
            self.assertEqual(copied["status"], "paused")
            self.assertEqual(copied["position_ms"], 42000)
            self.assertFalse(copied["scanning"])
            self.assertIsNone(copied["last_error"])
            self.assertEqual(before, (source / "state.db").read_bytes())

    def test_failed_publish_restores_all_existing_images(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staged, output = root / "staged", root / "output"
            staged.mkdir()
            output.mkdir()
            names = [name + ".png" for name, *_ in capture.PRESETS]
            for name in names:
                (staged / name).write_bytes(b"new")
                (output / name).write_bytes(b"old " + name.encode())
            replace = capture.os.replace
            calls = 0

            def fail_second(source, destination):
                nonlocal calls
                calls += 1
                if calls == 2:
                    raise OSError("simulated disk error")
                replace(source, destination)

            with patch.object(capture.os, "replace", side_effect=fail_second):
                with self.assertRaises(OSError):
                    capture.publish(staged, output)
            self.assertEqual(sorted(path.name for path in output.iterdir()), sorted(names))
            for name in names:
                self.assertEqual((output / name).read_bytes(), b"old " + name.encode())


if __name__ == "__main__":
    unittest.main()
