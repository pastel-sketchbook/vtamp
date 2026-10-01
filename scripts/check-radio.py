#!/usr/bin/env python3
"""Check native radio playback/reconnect in a muted private macOS server.

Requires a release build, macOS audio access, and installed FFmpeg solely to
generate silent HLS fixtures. Never contacts public stations or the user's server.
"""
import argparse
import functools
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]


def check(binary):
    if sys.platform != "darwin" or not shutil.which("ffmpeg"):
        raise RuntimeError("Needs macOS and FFmpeg for silent test fixtures")
    with tempfile.TemporaryDirectory(prefix="vtamp-radio-", dir="/tmp") as directory:
        work = Path(directory)
        media = work / "media"
        media.mkdir()
        subprocess.run([
            "ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
            "anullsrc=r=44100:cl=stereo", "-t", "180", "-c:a", "aac",
            "-b:a", "64k", "-f", "hls", "-hls_time", "2", "-hls_list_size", "0",
            "-hls_segment_filename", str(media / "part%03d.ts"), str(media / "fixture.m3u8"),
        ], check=True, timeout=45, capture_output=True)
        for suffix, codec in [("mp3", "libmp3lame"), ("aac", "aac")]:
            subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
                            "anullsrc=r=44100:cl=stereo", "-t", "8", "-c:a", codec,
                            str(media / ("silent." + suffix))], check=True, timeout=15, capture_output=True)
        started = time.monotonic()
        lock = threading.Lock()
        transport = {"offline": False, "requests": 0, "resolutions": 0}

        class Handler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                with lock:
                    transport["requests"] += 1
                    offline = transport["offline"]
                    if self.path == "/radio":
                        transport["resolutions"] += 1
                    token = transport["resolutions"]
                if offline:
                    self.send_error(503)
                elif self.path == "/radio":
                    self.send_response(302)
                    self.send_header("Location", f"/live.m3u8?token={token}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                elif self.path.startswith("/live.m3u8"):
                    if self.path != f"/live.m3u8?token={token}":
                        self.send_error(403)
                        return
                    sequence = min(80, int((time.monotonic() - started) / 2))
                    body = f"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{sequence}\n"
                    for index in range(sequence, sequence + 5):
                        body += f"#EXTINF:2.0,\npart{index:03d}.ts\n"
                    body = body.encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/vnd.apple.mpegurl")
                    self.send_header("Cache-Control", "no-store")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    super().do_GET()

        http_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(media)))
        thread = threading.Thread(target=http_server.serve_forever, daemon=True)
        thread.start()
        home = work / "home"
        env = dict(os.environ, VTAMP_HOME=str(home), VTAMP_MEDIA_KEYS="0")

        def call(*args):
            result = subprocess.run([str(binary), *args, "--json"], env=env, text=True, capture_output=True, timeout=8)
            reply = json.loads(result.stdout)
            if result.returncode or not reply["ok"]:
                raise RuntimeError(reply.get("error", result.stderr))
            return reply["data"]

        def wait_for(predicate, description, timeout=35):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                state = call("status")
                if predicate(state):
                    return state
                time.sleep(0.2)
            raise RuntimeError(f"Timed out waiting for {description}: {state.get('last_error')}")

        try:
            call("server", "start")
            call("volume", "0")
            url = f"http://127.0.0.1:{http_server.server_port}/radio"
            track = call("library", "stream", "add", url, "--name", "Silent HLS test")["tracks"][0]["id"]
            call("play", "--track", track)
            live = wait_for(lambda s: s["stream_status"] == "live", "native HLS playback")
            print("PASS: redirected HTTP HLS advances with native output muted", flush=True)
            with lock:
                transport["offline"] = True
            wait_for(lambda s: s["stream_status"] == "reconnecting", "reconnect after network loss", 50)
            with lock:
                transport["offline"] = False
            recovered = wait_for(lambda s: s["stream_status"] == "live", "network recovery")
            assert recovered["current_id"] == live["current_id"]
            assert recovered["queue_revision"] == live["queue_revision"]
            assert recovered["volume"] == 0 and recovered["position_ms"] == 0
            with lock:
                assert transport["resolutions"] >= 2
            print("PASS: reconnect resolves the original URL and preserves queue identity", flush=True)
            call("pause")
            time.sleep(1)
            with lock:
                count = transport["requests"]
            time.sleep(2)
            with lock:
                assert transport["requests"] == count, "Paused playback still downloads"
            call("server", "stop")
            call("server", "start")
            restored = call("status")
            assert restored["status"] == "paused" and restored["stream_status"] is None
            time.sleep(1)
            with lock:
                assert transport["requests"] == count, "Restart connected without resume"
            print("PASS: pause and restart release the connection", flush=True)
            call("resume")
            wait_for(lambda s: s["stream_status"] == "live", "resume")
            call("sleep", "set", "1s")
            wait_for(lambda s: s["status"] == "stopped", "sleep deadline", 5)
            print("PASS: resume and sleep deadline", flush=True)
            for suffix in ["mp3", "aac"]:
                address = f"http://127.0.0.1:{http_server.server_port}/silent.{suffix}"
                track = call("library", "stream", "add", address, "--name", suffix)["tracks"][0]["id"]
                call("play", "--track", track)
                wait_for(lambda s: s["stream_status"] == "live", f"HTTP {suffix} playback")
                call("stop")
                print(f"PASS: direct HTTP {suffix.upper()} advances with native output muted", flush=True)
        finally:
            try:
                subprocess.run([str(binary), "server", "stop"], env=env, capture_output=True, timeout=8)
            finally:
                http_server.shutdown()
                http_server.server_close()
                thread.join(timeout=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/vtamp")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(130))
    check(args.binary.resolve())
