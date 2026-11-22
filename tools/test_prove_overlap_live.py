"""Synthetic checks of the tap accounting in prove_overlap_live.epochs (REVIEW-FIXES-S10 R4).

Each case builds the stdout of a live run with one tap per framework epoch: head lines, owner yield
lines and `pending` audit events. An exemption must hold for its reason at that head, so a carried
F3 epoch and a tap lost after entering a lag visit from an unlisted map both fail.
"""
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import prove_overlap_live as live  # noqa: E402


def head(frame, epoch, taps, latched=False):
    """One head line; `taps` are the epochs whose tap the head received."""
    bits = 0
    for e in taps:
        bits |= 1 << (e % live.POOL)
    return (f'LIVE_EPOCH_HEAD frame={frame} epoch={epoch} taps={bits:x} clock={frame} '
            f'latched={"true" if latched else "false"}')


def pending(frame):
    return 'SIM_EVENT ' + json.dumps(dict(event='pending', frame=frame))


def run(lines):
    return live.epochs(lines, owner_frames=(3,))


# Heads 0..2 deliver epochs 0..2; head 2 retains frame 2. Head 3 presents it (epoch 3 begins),
# yields to the owner (a natural exit) and leaves; head 4 captures epoch 4 on the new map.
EXIT = [head(0, 0, [0]), head(1, 1, [1]), head(2, 2, [2]), pending(2),
        head(3, 2, []), 'LIVE_EPOCH_YIELD', head(4, 4, [4])]


class Epochs(unittest.TestCase):
    def test_exit_out_of_a_lag_visit_exempts_its_presented_epoch(self):
        summary = run(EXIT)
        self.assertEqual(summary['owner_consumed'], [3])
        self.assertEqual(summary['owner_heads'], [3])
        self.assertEqual(summary['lost'], [])

    def test_a_carried_f3_epoch_fails(self):
        # Regression: epoch 3 reaches the new map's first step.
        lines = EXIT[:-1] + [head(4, 4, [3, 4])]
        with self.assertRaises(AssertionError):
            run(lines)

    def test_a_tap_lost_without_a_retained_frame_before_the_head_is_lost(self):
        # Entering from an unlisted map: frame 2 was not retained, so head 3 presents nothing and
        # its epoch is not the owner's. Losing epoch 3 must be reported, not exempted.
        lines = [l for l in EXIT if l != pending(2)] + [pending(0)]
        lines[-2] = head(4, 4, [4])
        summary = run(lines)
        self.assertEqual(summary['owner_consumed'], [])
        self.assertEqual(summary['lost'], [3])

    def test_a_head_that_yields_and_runs_again_is_not_exempt(self):
        # A death retry: head 3 presents, yields, then runs frame 3 again and captures epoch 3.
        lines = EXIT[:-1] + [head(3, 3, [3]), head(4, 4, [4])]
        summary = run(lines)
        self.assertEqual(summary['owner_heads'], [])
        self.assertEqual(summary['lost'], [])
        # The same retry losing that epoch fails.
        lost = EXIT[:-1] + [head(3, 3, []), head(4, 4, [4])]
        self.assertEqual(run(lost)['lost'], [3])

    def test_a_head_that_never_yields_is_not_exempt(self):
        lines = [l for l in EXIT if l != 'LIVE_EPOCH_YIELD']
        self.assertEqual(run(lines)['lost'], [3])


if __name__ == '__main__':
    unittest.main()
