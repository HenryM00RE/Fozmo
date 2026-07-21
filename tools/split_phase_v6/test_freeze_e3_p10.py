from __future__ import annotations

import unittest

from .freeze_e3_p10 import SELECTED_IDENTIFIER


class FreezeE3P10Tests(unittest.TestCase):
    def test_freeze_keeps_research_identity_explicit(self) -> None:
        self.assertEqual(SELECTED_IDENTIFIER, "p10-moderate-05711")


if __name__ == "__main__":
    unittest.main()
