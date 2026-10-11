"""Video measurements include only the isolated pane's TUI and decoder."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('check_video', Path(__file__).with_name('check-video.py'))
check_video = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_video)


class VideoCpuTests(unittest.TestCase):
    def test_finds_tui_below_shell_and_ignores_other_listening_sessions(self):
        rows = [['10', '1', '/bin/zsh'], ['11', '10', '/bin/zsh'],
                ['12', '11', '/work/target/release/vtamp'],
                ['13', '12', '/opt/homebrew/bin/ffmpeg'],
                ['20', '1', '/other/vtamp'], ['21', '20', '/bin/ffmpeg']]
        with patch.object(check_video, 'process_table', return_value=rows):
            self.assertEqual(check_video.tui_process('10'), '12')
            self.assertEqual(check_video.tui_process('12'), '12')
            self.assertEqual(check_video.decoder_pid('12'), '13')
            with self.assertRaises(RuntimeError):
                check_video.decoder_pid('10')

    def test_samples_all_four_processes(self):
        expected = {'ghostty': 10.0, 'tmux': 2.0, 'tui': 3.0, 'ffmpeg': 4.0}
        with patch.object(check_video, 'decoder_pid', return_value='13'), \
                patch.object(check_video, 'sample_cpu', return_value=expected) as sample:
            self.assertEqual(check_video.sample_video_cpu('1', '2', '12'), expected)
            sample.assert_called_once_with(
                {'ghostty': '1', 'tmux': '2', 'tui': '12', 'ffmpeg': '13'})

    def test_restarted_decoder_invalidates_sample(self):
        with patch.object(check_video, 'decoder_pid', side_effect=['13', '14']), \
                patch.object(check_video, 'sample_cpu', return_value={}):
            with self.assertRaisesRegex(RuntimeError, 'decoder changed'):
                check_video.sample_video_cpu('1', '2', '12')


if __name__ == '__main__':
    unittest.main()
