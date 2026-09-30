#!/usr/bin/env python3
"""Opt-in macOS system-key smoke test using a private, muted server and generated audio."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import wave

ROOT = Path(__file__).resolve().parents[1]


def check(binary):
    if sys.platform != "darwin":
        raise RuntimeError("This check requires macOS and an interactive desktop session")
    if not binary.is_file():
        raise RuntimeError("Build vtamp first with cargo build --locked --release")
    with tempfile.TemporaryDirectory(prefix="vtamp-keys-", dir="/tmp") as directory:
        root = Path(directory)
        helper = root / "send-media-key"
        subprocess.run(["swiftc", "-module-cache-path", str(root / "swift-cache"),
                        str(ROOT / "scripts/send-media-key.swift"), "-o", str(helper)], check=True)
        env = dict(os.environ, VTAMP_HOME=str(root / "home"), VTAMP_MEDIA_KEYS="1", RUST_LOG="vtamp=debug")

        def command(*args):
            result = subprocess.run([str(binary), *args, "--json"], env=env,
                                    capture_output=True, text=True, timeout=15)
            if result.returncode:
                raise RuntimeError(result.stdout + result.stderr)
            return json.loads(result.stdout)["data"]

        def wait_for(predicate):
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                state = command("status")
                if state["last_error"]:
                    raise RuntimeError(state["last_error"])
                if predicate(state):
                    return state
                time.sleep(.05)
            raise RuntimeError("System key did not reach the test server; check Accessibility permission and the active Now Playing app")

        def wait_stopped():
            socket = root / "home/run/control.sock"
            deadline = time.monotonic() + 5
            while socket.exists() and time.monotonic() < deadline:
                time.sleep(.05)
            assert not socket.exists(), "Server failed to stop"

        files = []
        for name in ["vtamp system key check A", "vtamp system key check B"]:
            path = root / (name + ".wav")
            with wave.open(str(path), "wb") as audio:
                audio.setparams((1, 2, 8000, 0, "NONE", "not compressed"))
                audio.writeframes(b"\0\0" * 8000 * 60)
            files.append(str(path))
        tmux = ["tmux", "-S", str(root / "tmux.sock")]
        try:
            command("server", "start")
            command("volume", "0")
            state = command("play", *files)
            first, second = [item["id"] for item in state["queue"]]
            wait_for(lambda s: s["status"] == "playing" and s["position_ms"] > 100)
            def competing_server(_):
                result = subprocess.run([str(binary), "server", "run"], env=env,
                                        capture_output=True, text=True, timeout=10)
                assert result.returncode == 0, result.stderr
                assert "media controls registered" not in result.stdout + result.stderr
            with ThreadPoolExecutor(max_workers=3) as pool:
                list(pool.map(competing_server, range(3)))
            print("Competing servers exited without registering media handlers", flush=True)
            # Let macOS select the newly playing client before posting global keys.
            time.sleep(1)
            if shutil.which("tmux"):
                import shlex
                tui = shlex.join([str(binary), "--art", "none"])
                subprocess.run([*tmux, "-f", "/dev/null", "new-session", "-d", "-s", "check", tui],
                               env=env, check=True, capture_output=True)
                time.sleep(.5)
                subprocess.run([*tmux, "send-keys", "-t", "check", "q"], check=True, capture_output=True)
                time.sleep(.3)
                assert command("status")["status"] == "playing"
                print("TUI detach preserved playback", flush=True)
            for key, predicate in [
                ("toggle", lambda s: s["status"] == "paused"),
                ("toggle", lambda s: s["status"] == "playing"),
                ("next", lambda s: s["current_id"] == second),
                ("previous", lambda s: s["current_id"] == first),
            ]:
                subprocess.run([str(helper), key], check=True)
                wait_for(predicate)
                print(f"System media key: {key} passed", flush=True)
            command("seek", "59.9")
            wait_for(lambda s: s["current_id"] == second)
            print("Natural track advancement passed", flush=True)
            command("server", "stop")
            wait_stopped()
            command("server", "start")
            assert command("status")["status"] == "paused"
            print("Server restart restored paused playback", flush=True)
            command("server", "stop")
            wait_stopped()
            foreground = subprocess.Popen([str(binary), "server", "run"], env=env,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                deadline = time.monotonic() + 5
                while not (root / "home/run/control.sock").exists() and time.monotonic() < deadline:
                    time.sleep(.05)
                assert command("status")["status"] == "paused"
                time.sleep(.1)
                foreground.terminate()
                output, error = foreground.communicate(timeout=10)
                assert foreground.returncode == 0, output + error
                wait_stopped()
                print("SIGTERM released the server and AppKit event loop", flush=True)
            finally:
                if foreground.poll() is None:
                    foreground.kill()
                    foreground.communicate(timeout=5)
        except Exception:
            log = root / "home/server.log"
            if log.exists():
                print(log.read_text()[-6000:], file=sys.stderr)
            raise
        finally:
            subprocess.run([str(binary), "server", "stop"], env=env, capture_output=True, timeout=10)
            if shutil.which("tmux"):
                subprocess.run([*tmux, "kill-server"], capture_output=True, timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/vtamp")
    args = parser.parse_args()
    check(args.binary.resolve())
