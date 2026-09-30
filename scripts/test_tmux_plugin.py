"""Exercise the plugin on a private tmux server, never the caller's session."""
import fcntl
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import struct
import subprocess
import tempfile
import termios
import time
import unittest

REPO = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which("tmux"), "tmux is required")
class TmuxPluginTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="vtamp-plugin-", dir="/tmp")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.socket = str(self.root / "tmux.sock")
        self.env = dict(os.environ, TERM="xterm-256color")
        self.env.pop("TMUX", None)
        self.env.pop("TMUX_PANE", None)
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "test", "-x", "180", "-y", "24", "/bin/sh")
        self.addCleanup(lambda: self.tmux("kill-server", check=False))
        self.env["TMUX"] = self.tmux("display-message", "-p", "#{socket_path},#{pid},0").strip()
        # Spaces, quotes, hashes, and parentheses must survive both tmux and sh.
        self.plugin = self.root / "plugin ' # & (copy)"
        (self.plugin / "scripts").mkdir(parents=True)
        shutil.copy2(REPO / "vtamp.tmux", self.plugin / "vtamp.tmux")
        shutil.copy2(REPO / "scripts/tmux-status.sh", self.plugin / "scripts/tmux-status.sh")
        self.binary = self.root / "player ' # (test)"
        self.binary.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
        self.binary.chmod(0o755)
        self.tmux("set-option", "-g", "@vtamp-bin", str(self.binary))

    def tmux(self, *args, check=True):
        return subprocess.run(["tmux", "-S", self.socket, *args], env=self.env,
                              text=True, capture_output=True, check=check, timeout=5).stdout

    def load(self):
        subprocess.run([str(self.plugin / "vtamp.tmux")], env=self.env,
                       check=True, capture_output=True, timeout=5)

    def test_reload_preserves_layout_and_handles_session_overrides(self):
        right = '#{vtamp} | %H:%M #{pane_title}'
        self.tmux("set-option", "-g", "status-right", right)
        self.tmux("set-option", "-g", "status-left", "left #{vtamp}")
        self.tmux("set-option", "-t", "test", "status-right", "local #{vtamp} %H:%M")
        interval = self.tmux("show-options", "-gv", "status-interval")
        width = self.tmux("show-options", "-gv", "status-right-length")
        self.load()
        global_format = self.tmux("show-options", "-gv", "status-right")
        local_format = self.tmux("show-options", "-t", "test", "-v", "status-right")
        self.assertNotIn("#{vtamp}", global_format)
        self.assertTrue(global_format.endswith(" | %H:%M #{pane_title}\n"))
        self.assertTrue(local_format.startswith("local #(exec "))
        self.assertIn("#(exec ", self.tmux("show-options", "-gv", "status-left"))
        self.assertEqual(self.tmux("show-options", "-t", "test", "-qv", "status-left"), "")
        self.load()
        self.assertEqual(global_format, self.tmux("show-options", "-gv", "status-right"))
        self.assertEqual(local_format, self.tmux("show-options", "-t", "test", "-v", "status-right"))
        self.assertEqual(interval, self.tmux("show-options", "-gv", "status-interval"))
        self.assertEqual(width, self.tmux("show-options", "-gv", "status-right-length"))

    def test_wrapper_options_and_missing_binary(self):
        def output():
            result = subprocess.run([str(self.plugin / "scripts/tmux-status.sh"), self.socket],
                                    env=self.env, text=True, capture_output=True, check=True, timeout=5)
            self.assertEqual(result.stderr, "")
            return result.stdout
        self.assertEqual(output(), "tmux\nstatus\n--max-width\n50\n")
        self.tmux("set-option", "-g", "@vtamp-max-width", "030")
        self.tmux("set-option", "-g", "@vtamp-show-artist", "on")
        self.assertEqual(output(), "tmux\nstatus\n--max-width\n30\n--show-artist\n")
        self.tmux("set-option", "-g", "@vtamp-max-width", "$(exit 1)")
        self.assertIn("\n50\n", output())
        self.tmux("set-option", "-g", "@vtamp-bin", str(self.root / "missing"))
        self.assertEqual(output(), "\n")

    def test_actual_status_job_runs_from_quoted_path_and_displays_literal_metadata(self):
        state = self.root / "state.txt"
        state.write_text("▶ A ##[fg=red] ##{pane_id} ##(false) · 0:56 / 3:39\n")
        self.binary.write_text(f"#!/bin/sh\ncat {shlex.quote(str(state))}\n")
        self.tmux("set-option", "-g", "status-right", "#{vtamp} CLOCK")
        self.tmux("set-option", "-g", "status-right-length", "150")
        self.tmux("set-option", "-g", "status-interval", "1")
        self.load()
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        self.addCleanup(os.close, slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 180, 0, 0))
        client = subprocess.Popen(["tmux", "-S", self.socket, "attach-session", "-t", "test"],
                                  env=self.env, stdin=slave, stdout=slave, stderr=slave)
        def stop():
            client.terminate()
            client.wait(timeout=5)
        self.addCleanup(stop)
        deadline = time.monotonic() + 5
        rendered = b""
        expected = "▶ A #[fg=red] #{pane_id} #(false) · 0:56 / 3:39 CLOCK".encode()
        while time.monotonic() < deadline and expected not in rendered:
            if select.select([master], [], [], 0.2)[0]:
                rendered += os.read(master, 65536)
        self.assertIn(expected, rendered)

        state.write_text("Ⅱ Paused track · 0:56 / 3:39\n")
        rendered = b""
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline and b"Paused track" not in rendered:
            if select.select([master], [], [], 0.2)[0]:
                rendered += os.read(master, 65536)
        self.assertIn("Ⅱ Paused track · 0:56 / 3:39 CLOCK".encode(), rendered)

        state.write_text("\n")
        # Let the empty result replace the old job output, then request a full frame.
        time.sleep(2)
        while select.select([master], [], [], 0)[0]:
            os.read(master, 65536)
        self.tmux("refresh-client", "-t", os.ttyname(slave))
        rendered = b""
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline and b"CLOCK" not in rendered:
            if select.select([master], [], [], 0.2)[0]:
                rendered += os.read(master, 65536)
        self.assertIn(b"CLOCK", rendered)
        self.assertNotIn(b"Paused track", rendered)


if __name__ == "__main__":
    unittest.main()
