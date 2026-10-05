# tmux video transport

Local tmux Kitty video uses temporary files by default. This avoids the bulk PTY
upload path that caused sustained slow playback after some pane swaps in the
maintainer's Ghostty setup. Repeated debug and release tests did not reproduce
that sustained slowdown after the file-lifetime fix. The underlying PTY/tmux
throughput problem has not been identified conclusively.

## Selection and file lifetime

- `VTAMP_KITTY_VIDEO_FILE=0` selects direct transmission.
- With no override, local tmux Kitty uses files. SSH environments identified by
  `SSH_CONNECTION`, `SSH_CLIENT`, or `SSH_TTY` use direct transmission.
- `VTAMP_KITTY_VIDEO_FILE=1` enables files when the terminal and TUI share a
  filesystem. A tmux server originally started locally may retain a local
  environment when a remote client attaches; use `0` in that situation.
- Other protocols and Kitty outside tmux retain their existing transport. A
  failed temporary-directory setup falls back to direct transmission.

The video worker writes original RGBA pixels to a private temporary directory.
Kitty `t=t` commands carry only the base64-encoded file path and image dimensions;
the usual virtual placement and Unicode placeholders position the picture. No
resolution reduction is involved. Ghostty 1.3.1 supports this transfer method.

The terminal removes consumed files. The worker removes abandoned frames and
reclaims retired, unconsumed files after a two-second grace period. At the cap
of 32 files, it first reclaims a retired frame. Current UI/mailbox frames are
protected. This matters because tmux can discard a hidden pane's passthrough
command, leaving a file that the terminal will never consume. The worker removes
the directory on shutdown; the UI thread only marks handoff and performs no file
I/O for this transport.

## Observations from the investigation

The maintainer's rotation used two sequential `swap-pane` calls without `-d`,
moving video between a parked window and a slot in a multi-pane window. In
Ghostty 1.3.1 with tmux next-3.9, video and spectrum sometimes slowed to roughly
1 fps while audio continued. A keypress or focus change could restore updates.
Earlier versions also exposed the cursor at the outer terminal's origin.

| Measurement | Bulk pixel transmission | File transmission after cleanup fix |
| --- | --- | --- |
| Typical upload time in the normal state | About 7 ms | 0.018 ms median in the final captured run |
| Upload during a slow interval | About 0.75–1.3 s | Small commands remained below 0.041 ms in that run |
| Transfer size | Hundreds of KB per frame | Usually 272 bytes per frame in the tested environment |
| Pending file count | Not applicable | At most 15 in that run, below the cap of 32 |

During a symbolized two-second slow-state capture, 1178 of 1433 TUI main-thread
samples were inside the kernel `write()` beneath `upload_graphics`, rather than
waiting for a Rust stdout lock. Ghostty's input reader waited in `poll()` in
1713 of 1729 samples. tmux spent 634 of 1717 samples in pane-activity dispatch.
This localizes the delay to delivery through the PTY/tmux path, but does not prove
that activity hooks or a particular tmux function caused it.

Moving uploads before text drawing removed cursor flicker. Removing pane
synchronization, splitting writes into 16 KiB pieces, and pausing output for
150 ms after a slow transfer did not resolve the sustained slowdown. The output
pause was removed. A nonblocking `/dev/tty` experiment exited during its first
readiness wait and was reverted; socket tests had not established PTY compatibility.

The first file-transfer run stopped after roughly 145 seconds. Retired files
could accumulate when hidden-pane commands were dropped; cleanup and queue
pressure handling were added. The maintainer subsequently reported more than
three minutes and 15 swaps without recurrence, followed by additional successful
debug and release tests. The latest captured debug session itself spans 137 s
and contains 1497 small uploads, no file-limit errors, and no video error packets
delivered to the UI. These are bounded observations in one local environment,
not a guarantee for other terminals or remote sessions.

## Remaining limitation and diagnostics

Covers still use direct pixel uploads and can briefly stall during swap/resize;
the final captured run included cover transfers taking about 1.14 s. Direct
video transmission can still exhibit the original throughput issue.

`VTAMP_TUI_TRACE=/tmp/vtamp-tui-trace.jsonl` enables optional timing records.
Use a debug client for symbolized stack sampling. Compare `output.upload` and
`output.write` durations with `video.encode`, `video.ready`, and `video.accept`;
`video.file_queue`, `video.file_limit`, and `video.error` distinguish file-lifetime
failures from cancelled or failed decoder work. An internal error associated
with a cancelled request need not be delivered to the UI. See the
[README tracing instructions](../README.md#trace-a-tui-stall).
