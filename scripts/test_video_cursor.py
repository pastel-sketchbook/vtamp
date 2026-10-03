"""Cursor exposure is measured at the outer terminal, beyond pane snapshots."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('check_video', Path(__file__).with_name('check-video.py'))
check_video = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_video)


class CursorTraceTests(unittest.TestCase):
    def test_passthrough_cursor_reset_is_exposed_without_outer_update(self):
        trace = b'\x1b_Ga=T;AAAA\x1b\\\x1b[?25h\x1b[1;1H'
        self.assertEqual(len(check_video.exposed_cursor_moves(trace)), 1)

    def test_outer_update_hides_resets_until_caret_is_restored(self):
        trace = (b'\x1b[?2026h\x1b_Ga=T;AAAA\x1b\\\x1b[?25h\x1b[1;1H'
                 b'\x1b[?25l\x1b[12;47H\x1b[?25h\x1b[?2026l')
        self.assertEqual(check_video.exposed_cursor_moves(trace), [])
        self.assertEqual(check_video.cursor_trace(trace)['released_cursors'], [[46, 11]])

    def test_ending_an_update_at_the_origin_exposes_the_wrong_caret(self):
        trace = b'\x1b[?2026h\x1b[?25h\x1b[1;1H\x1b[?2026l'
        self.assertEqual(check_video.cursor_trace(trace)['released_cursors'], [[0, 0]])

    def test_measurement_range_keeps_prior_cursor_and_sync_state(self):
        before = b'\x1b[?25h\x1b[H\x1b[?2026h'
        during = b'\x1b[1;1H\x1b[?2026l\x1b[12;47H'
        after = b'\x1b[H'
        moves = check_video.exposed_cursor_moves(before + during + after,
                                                len(before), len(before + during))
        self.assertEqual([move['move'] for move in moves], ['\x1b[12;47H'])


if __name__ == '__main__':
    unittest.main()
